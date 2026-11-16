//! A conversation for the hub tests: main and subagents answer from separate
//! scripts ([`super::script`]), a host that records what the hub tells it, and
//! a main loop that runs what reaches its inbox.

use std::{
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use async_trait::async_trait;
use haest_core::{
    Agent, AgentDeps, AgentOptions, LoopEvent, Message,
    ledger::{ConversationCaps, Ledger, LedgerHooks, LedgerSnapshot},
    provider::{Effort, ModelError, ResolvedModel},
    tool::{JsonMap, Tool, ToolContext, ToolError, ToolOutput},
};
use serde_json::Value;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};

pub use super::script::{CALL_USD, Script, call, now, say, spawns};
use super::{
    super::{
        AgentRef, AgentToolOptions, SubagentCaps, SubagentEvent, SubagentHub, SubagentParent,
        SubagentTypes,
    },
    script::Roles,
};
use crate::{GlobTool, inbox::Inbox, testkit::TestWorkspace};

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

struct Quiet;
impl LedgerHooks for Quiet {
    fn persist(&self, _: &LedgerSnapshot) {}
    fn warn(&self, _: &str) {}
}

pub struct FakeParent {
    pub provider: Arc<Roles>,
    ws: Arc<TestWorkspace>,
    pub ledger: Arc<Mutex<Ledger>>,
    tx: UnboundedSender<String>,
    pub activity: Mutex<Vec<usize>>,
    /// (agent id, event kind) in order.
    pub events: Mutex<Vec<(String, &'static str)>>,
    /// The host's kinds of subagent: `explore` (no tools of its own, the
    /// default) and `general` (the base tools); None = no kinds.
    kinds: Option<SubagentTypes>,
}

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
        let _ = self.tx.send(text);
    }
}

#[async_trait]
impl SubagentParent for FakeParent {
    fn deps(&self) -> AgentDeps {
        let p = self.provider.clone();
        let models = move |spec: &str| {
            let model = spec.split_once(':').map_or(spec, |(_, m)| m).to_string();
            Ok::<_, ModelError>(ResolvedModel {
                provider: p.clone(),
                model,
            })
        };
        let mut d = AgentDeps::new(
            Arc::new(models),
            Arc::new(self.base_tools()),
            Arc::new("main".to_string()),
            "/p",
            self.ws.clone(),
        );
        d.ledger = Some(self.ledger.clone());
        d
    }
    fn base_tools(&self) -> Vec<Arc<dyn Tool>> {
        vec![Arc::new(GlobTool), Arc::new(TaskCreate)]
    }
    async fn system_prompt(&self, model_spec: &str, tools: &[Arc<dyn Tool>]) -> String {
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
            types: self.kinds.clone(),
            ..Default::default()
        }
    }
    fn subagent_tools(&self, subagent_type: Option<&str>) -> Vec<Arc<dyn Tool>> {
        match subagent_type {
            Some("explore") => Vec::new(),
            _ => self.base_tools(),
        }
    }
    fn on_activity(&self, running: &[String]) {
        self.activity.lock().unwrap().push(running.len());
    }
    fn on_subagent(&self, id: &str, event: SubagentEvent) {
        let kind = match event {
            SubagentEvent::Started { .. } => "started",
            SubagentEvent::Loop(_) => "loop",
            SubagentEvent::InnerCall(_) => "inner",
            SubagentEvent::Stopped { .. } => "stopped",
        };
        self.events.lock().unwrap().push((id.to_string(), kind));
    }
}

pub struct Rig {
    pub hub: Arc<SubagentHub>,
    pub parent: Arc<FakeParent>,
    pub main: Agent,
    pub events: Vec<LoopEvent>,
    pub inbox: UnboundedReceiver<String>,
}

impl Rig {
    pub fn new(main: Script, sub: Script, caps: SubagentCaps) -> Self {
        Self::with_kinds(main, sub, caps, None)
    }

    /// A conversation whose host has `kinds` of subagent.
    pub fn with_kinds(
        main: Script,
        sub: Script,
        caps: SubagentCaps,
        kinds: Option<SubagentTypes>,
    ) -> Self {
        let provider = Arc::new(Roles {
            main,
            sub,
            calls: Mutex::default(),
            sub_tools: Mutex::default(),
            sub_systems: Mutex::default(),
        });
        let (tx, inbox) = unbounded_channel();
        let parent = Arc::new(FakeParent {
            provider,
            ws: TestWorkspace::with_files([("/p/a.txt", "a")]),
            ledger: Arc::new(Mutex::new(Ledger::new(
                ConversationCaps::default(),
                Arc::new(Quiet),
            ))),
            tx,
            activity: Mutex::default(),
            events: Mutex::default(),
            kinds,
        });
        let hub = SubagentHub::new(parent.clone(), caps);
        let mut tools = parent.base_tools();
        tools.extend(hub.tools_for(&AgentRef::main()));
        let deps = AgentDeps {
            tools: Arc::new(tools),
            ..parent.deps()
        };
        let main = Agent::new(deps, AgentOptions::new("fake:main-model"));
        Self {
            hub,
            parent,
            main,
            events: Vec::new(),
            inbox,
        }
    }

    /// The tool names each subagent's model calls offered, in call order.
    pub fn provider_sub_tools(&self) -> Vec<Vec<String>> {
        self.parent.provider.sub_tools.lock().unwrap().clone()
    }

    pub fn main_tools(&self) -> Vec<String> {
        self.main
            .deps()
            .tools
            .tools()
            .iter()
            .map(|t| t.name().to_string())
            .collect()
    }

    pub async fn turn(&mut self, text: &str) {
        self.main.run(text, &mut self.events).await;
    }

    /// Run whatever reached main's inbox as turns until `done` holds (5s at most).
    pub async fn settle(&mut self, done: impl Fn(&Rig) -> bool) {
        let end = Instant::now() + Duration::from_secs(5);
        while !done(self) && Instant::now() < end {
            match self.inbox.try_recv() {
                Ok(text) => self.turn(&text).await,
                Err(_) => tokio::time::sleep(Duration::from_millis(10)).await,
            }
        }
    }

    /// Main's user and tool-result texts, in order.
    pub fn texts(&self) -> Vec<String> {
        texts(self.main.history())
    }

    pub fn tool_results(&self) -> Vec<(String, bool)> {
        let results = self.main.history().iter().filter_map(|m| match m {
            Message::Tool(t) => Some((t.content.clone(), t.is_error)),
            _ => None,
        });
        results.collect()
    }

    pub fn ids(&self, kind: &str) -> Vec<String> {
        let events = self.parent.events.lock().unwrap();
        events
            .iter()
            .filter(|(_, k)| *k == kind)
            .map(|(id, _)| id.clone())
            .collect()
    }
}

pub fn texts(messages: &[Message]) -> Vec<String> {
    let texts = messages.iter().filter_map(|m| match m {
        Message::User(u) => Some(u.content.clone()),
        Message::Tool(t) => Some(t.content.clone()),
        Message::Assistant(_) => None,
    });
    texts.collect()
}
