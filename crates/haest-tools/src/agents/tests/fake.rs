//! The hub tests' host: a parent that records what the hub tells it (the
//! running set, every event, every message, what reaches main and how) and
//! may implement the hooks a host can (kinds, member runs, its own note).

use std::sync::{Arc, Mutex, OnceLock, Weak};

use async_trait::async_trait;
use haest_core::{
    LoopEvent, SharedDeps, ToolRef, Tools,
    ledger::{Ledger, LedgerHooks, LedgerSnapshot},
    provider::{Effort, ModelError, ResolvedModel},
    tool::{JsonMap, Tool, ToolContext, ToolError, ToolOutput},
};
use serde_json::Value;
use tokio::sync::mpsc::UnboundedSender;

use super::{lifecycle::Lifecycle, script::Roles};
use crate::{
    GlobTool,
    agents::{
        AgentError, AgentToolOptions, InProcessRun, MemberRun, MemberStart, Sender, SubagentEvent,
        SubagentHub, SubagentParent, SubagentTypes, subagent_note,
    },
    inbox::Inbox,
    testkit::TestWorkspace,
};

/// A tool only main may have.
struct TaskCreate;

#[async_trait]
impl Tool for TaskCreate {
    fn name(&self) -> &str {
        "TaskCreate"
    }
    fn description(&self) -> String {
        "x".into()
    }
    fn parameters(&self) -> Value {
        serde_json::json!({"type": "object"})
    }
    fn kind(&self) -> &str {
        "task.create"
    }
    async fn execute(&self, _: JsonMap, _: &ToolContext) -> Result<ToolOutput, ToolError> {
        Ok(ToolOutput::text("ok"))
    }
}

pub struct Quiet;
impl LedgerHooks for Quiet {
    fn persist(&self, _: &LedgerSnapshot) {}
    fn warn(&self, _: &str) {}
}

/// What reached main, and through which door of its inbox.
#[derive(Debug, Clone, PartialEq)]
pub enum ForMain {
    /// [`Inbox::submit`]: a message.
    Message(String),
    /// [`Inbox::submit_answer`]: a background answer.
    Answer(String),
}

/// One [`SubagentEvent::Message`] as the parent heard it.
#[derive(Debug, Clone, PartialEq)]
pub struct Heard {
    /// The id it was keyed by.
    pub id: String,
    pub from: Sender,
    pub to: String,
    pub text: String,
}

/// What the host does beyond haest's defaults.
#[derive(Default)]
pub struct Host {
    /// Its kinds of subagent: `explore` (no tools of its own, the default)
    /// and `general` (the base tools); None = no kinds.
    pub kinds: Option<SubagentTypes>,
    /// What it does at a member's start; None = haest's default.
    pub lifecycle: Option<Arc<Lifecycle>>,
    /// Its own note for a member; None = haest's.
    pub note: Option<fn(&MemberStart) -> String>,
}

pub struct FakeParent {
    pub provider: Arc<Roles>,
    pub ws: Arc<TestWorkspace>,
    pub ledger: Arc<Mutex<Ledger>>,
    pub tx: UnboundedSender<String>,
    pub activity: Mutex<Vec<usize>>,
    /// (agent id, event kind) in order.
    pub events: Mutex<Vec<(String, &'static str)>>,
    pub heard: Mutex<Vec<Heard>>,
    /// Everything that reached main's inbox, in order; all of it also runs
    /// as main's turns ([`super::support::Rig::settle`]).
    pub for_main: Mutex<Vec<ForMain>>,
    /// The hub over this parent, to look at as an answer lands.
    pub hub: OnceLock<Weak<SubagentHub>>,
    /// Whether the hub called itself idle as each answer landed.
    pub idle_at_answer: Mutex<Vec<bool>>,
    pub host: Host,
    /// What a test changes in the conversation's deps
    /// ([`super::support::Rig::adjust`]).
    pub adjust: Mutex<Option<Box<Adjust>>>,
}

type Adjust = dyn Fn(SharedDeps) -> SharedDeps + Send + Sync;

/// The kinds a host with kinds has: `explore` (the default) and `general`.
pub fn explore_or_general() -> SubagentTypes {
    SubagentTypes {
        names: vec!["explore".into(), "general".into()],
        description: "explore only looks".into(),
        default: "explore".into(),
    }
}

impl Inbox for FakeParent {
    fn submit(&self, text: String) {
        self.for_main
            .lock()
            .unwrap()
            .push(ForMain::Message(text.clone()));
        let _ = self.tx.send(text);
    }
    fn submit_answer(&self, text: String) {
        if let Some(hub) = self.hub.get().and_then(Weak::upgrade) {
            self.idle_at_answer.lock().unwrap().push(hub.idle());
        }
        self.for_main
            .lock()
            .unwrap()
            .push(ForMain::Answer(text.clone()));
        let _ = self.tx.send(text);
    }
}

#[async_trait]
impl SubagentParent for FakeParent {
    fn deps(&self) -> SharedDeps {
        let p = self.provider.clone();
        let models = move |spec: &str| {
            let model = spec.split_once(':').map_or(spec, |(_, m)| m).to_string();
            Ok::<_, ModelError>(ResolvedModel::new(p.clone(), model))
        };
        let d = SharedDeps::new(models, "/p", self.ws.clone()).with_ledger(self.ledger.clone());
        match &*self.adjust.lock().unwrap() {
            Some(adjust) => adjust(d),
            None => d,
        }
    }
    fn base_tools(&self) -> Tools {
        Tools::from_iter([ToolRef::new(GlobTool), ToolRef::new(TaskCreate)])
    }
    async fn system_prompt(&self, model_spec: &str, tools: &[ToolRef]) -> String {
        let names: Vec<&str> = tools.iter().map(|t| t.name()).collect();
        format!(
            "You are haest.\n- Model: {model_spec}\n- Tools: {}",
            names.join(", ")
        )
    }
    fn default_model(&self) -> (String, Option<Effort>) {
        ("fake:main-model".into(), None)
    }
    fn agent_tool_options(&self) -> AgentToolOptions {
        AgentToolOptions {
            types: self.host.kinds.clone(),
            ..Default::default()
        }
    }
    fn subagent_tools(&self, subagent_type: Option<&str>) -> Tools {
        match subagent_type {
            Some("explore") => Tools::new(),
            _ => self.base_tools(),
        }
    }
    fn on_activity(&self, running: &[String]) {
        self.activity.lock().unwrap().push(running.len());
    }
    async fn start_member(
        &self,
        start: &MemberStart,
        _cancel: &tokio_util::sync::CancellationToken,
    ) -> Result<Box<dyn MemberRun>, AgentError> {
        match &self.host.lifecycle {
            Some(l) => l.start(start, &self.provider, self).await,
            None => Ok(Box::new(InProcessRun)),
        }
    }
    fn subagent_note(&self, member: &MemberStart) -> String {
        match self.host.note {
            Some(note) => note(member),
            // What the trait's default says.
            None => subagent_note(
                &member.name,
                &member.parent,
                &member.input.description,
                &member.caps,
            ),
        }
    }
    fn on_subagent(&self, id: &str, event: SubagentEvent) {
        let kind = match event {
            SubagentEvent::Started { .. } => "started",
            SubagentEvent::Loop(LoopEvent::Guard { .. }) => "guard",
            SubagentEvent::Loop(_) => "loop",
            SubagentEvent::InnerCall(_) => "inner",
            SubagentEvent::Stopped { .. } => "stopped",
            SubagentEvent::Message { from, to, text } => {
                let id = id.to_string();
                let heard = Heard { id, from, to, text };
                self.heard.lock().unwrap().push(heard);
                "message"
            }
        };
        self.events.lock().unwrap().push((id.to_string(), kind));
    }
}
