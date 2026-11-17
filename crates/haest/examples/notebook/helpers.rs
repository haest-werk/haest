//! The notebook's helpers on haest's own hub: each one runs on a node run of
//! its own (its token, the model the node resolved, the node's caps), keeps
//! its messages as that run's records, and the node closes the run when it
//! ends. The panel hears every message, main hears a message at once and an
//! answer when every helper is done, and every helper reads the notebook's
//! own note.
//!
//! PLUG POINTS: `SubagentParent` (with `start_member`, `subagent_note`,
//! `on_subagent`), `Inbox` (`submit`, `submit_answer`), `MemberRun`, and the
//! `SubagentHub` over them; `AgentToolOptions` (the notebook's kinds, its
//! model text).

use std::{
    sync::{Arc, Mutex, MutexGuard},
    time::Duration,
};

use haest::{
    CancellationToken, Effort, SharedDeps, ToolRef, Tools, WorkspaceRef, async_trait,
    core::caps::SubagentCaps,
    tools::{
        AgentError, AgentToolOptions, Ending, Inbox, Member, MemberCaps, MemberEnd, MemberRun,
        MemberStart, Sender, SubagentEvent, SubagentParent,
    },
};

use crate::{
    MODEL, T0, agent_options,
    gateway::Gateway,
    host::{NodeSessions, NotebookHost, RunClock},
    shared,
};

/// What an explore helper gets of the run's tools: finding and reading.
const EXPLORE: [&str; 4] = ["Read", "Glob", "Grep", "notebook_search"];

/// The model the node gives a helper that names "haiku".
pub const HELPER_MODEL: &str = "gateway:anthropic/claude-haiku-5";

/// The node's caps on one helper run.
pub const HELPER_CAPS: MemberCaps = MemberCaps {
    usd: 0.25,
    steps: 10,
    time: Duration::from_secs(120),
};

/// The caps on the notebook's helper tree: one level deep, two at once.
pub fn tree_caps() -> SubagentCaps {
    SubagentCaps {
        max_depth: 1,
        max_concurrent: 2,
        ..SubagentCaps::default()
    }
}

/// One helper run the node started.
#[derive(Debug, Clone, PartialEq)]
pub struct Started {
    pub name: String,
    pub kind: Option<String>,
    pub asked_model: Option<String>,
    pub token: String,
}

/// One helper run the node closed: its answer, how it ended, what it cost.
#[derive(Debug, Clone, PartialEq)]
pub struct Closed {
    pub token: String,
    pub ending: Ending,
    pub answer: String,
    pub usd: f64,
    pub model_calls: u32,
    /// The records the run kept before it closed.
    pub records: usize,
}

/// The node's helper-run endpoint, faked: it starts and closes helper runs.
#[derive(Default)]
pub struct Node {
    started: Mutex<Vec<Started>>,
    closed: Mutex<Vec<Closed>>,
}

impl Node {
    /// The helper runs the node started, oldest first.
    pub fn started(&self) -> Vec<Started> {
        locked(&self.started).clone()
    }

    /// The helper runs the node closed, oldest first.
    pub fn closed(&self) -> Vec<Closed> {
        locked(&self.closed).clone()
    }
}

/// A message to a helper or main, for the panel.
#[derive(Debug, Clone, PartialEq)]
pub struct AgentMessage {
    pub from: String,
    pub to: String,
    pub text: String,
    /// The person wrote it, not an agent.
    pub person: bool,
}

/// What reaches main, and how.
#[derive(Debug, Clone, PartialEq)]
pub enum ForMain {
    /// Main hears it at once.
    Message(String),
    /// A helper's answer, held until every helper is done.
    Answer(String),
}

/// What the panel sees of the run's helpers.
#[derive(Default)]
pub struct Panel {
    messages: Mutex<Vec<AgentMessage>>,
    main: Mutex<Vec<ForMain>>,
}

impl Panel {
    /// Every message between the agents (and the person), oldest first.
    pub fn messages(&self) -> Vec<AgentMessage> {
        locked(&self.messages).clone()
    }

    /// What reached main, oldest first.
    pub fn main(&self) -> Vec<ForMain> {
        locked(&self.main).clone()
    }
}

/// The node's and the panel's lists. Only this file's code runs under these
/// locks, so a poisoned one is a bug here.
fn locked<T>(list: &Mutex<T>) -> MutexGuard<'_, T> {
    list.lock().expect("a helper list's lock poisoned")
}

/// The run's side of haest's hub.
pub struct NodeHelpers {
    pub gateway: Arc<Gateway>,
    /// The run's tools, without the agent tools.
    pub base: Tools,
    pub workspace: WorkspaceRef,
    /// The run's own host: its clock, records and caps.
    pub host: Arc<NotebookHost>,
    pub node: Arc<Node>,
    pub panel: Arc<Panel>,
}

impl Inbox for NodeHelpers {
    /// A message for main (SendMessage, the person): main hears it now.
    fn submit(&self, text: String) {
        locked(&self.panel.main).push(ForMain::Message(text));
    }
    /// A background helper's answer: held for main's next turn.
    fn submit_answer(&self, text: String) {
        locked(&self.panel.main).push(ForMain::Answer(text));
    }
}

/// The notebook's own note: it names the person, and tells an explore helper
/// it only reads.
fn notebook_note(m: &MemberStart) -> String {
    let reads = if m.input.subagent_type.as_deref() == Some("explore") {
        "\n\nYou only read and search: you have no tools that change files. Report what you found, with paths, so the agent that started you can act on it."
    } else {
        ""
    };
    format!(
        "# You are a sub-agent\n\nYou are agent \"{}\", started by \"{}\" for: {}\n\nDo the task, then end your turn with your COMPLETE answer: your final message is returned to \"{}\" verbatim, and nobody else reads your transcript. Nobody will answer questions: if something is ambiguous, take the sensible reading and say which one you took.\n\nYour caps: ${}, {} model calls, {} seconds. Messages from other agents (or from the person) arrive as user messages starting with [Message from ...]; reach a running agent (or \"main\", the agent that talks with the person) with SendMessage.{reads}",
        m.name,
        m.parent,
        m.input.description,
        m.parent,
        m.caps.usd,
        m.caps.steps,
        m.caps.time.as_secs()
    )
}

#[async_trait]
impl SubagentParent for NodeHelpers {
    fn deps(&self) -> SharedDeps {
        shared(&self.gateway, "", self.workspace.clone(), self.host.clone())
    }
    fn base_tools(&self) -> Tools {
        self.base.clone()
    }
    async fn system_prompt(&self, _model: &str, tools: &[ToolRef]) -> String {
        let reads_only = !tools
            .iter()
            .any(|t| t.name() == "Write" || t.name() == "Edit");
        let rules = "You help inside a person's notebook.";
        if reads_only {
            format!("{rules} You only read: report what you found, with paths.")
        } else {
            rules.into()
        }
    }
    fn default_model(&self) -> (String, Option<Effort>) {
        (MODEL.into(), None)
    }
    fn agent_tool_options(&self) -> AgentToolOptions {
        agent_options()
    }
    fn subagent_tools(&self, kind: Option<&str>) -> Tools {
        let all = kind == Some("general");
        let tools = self
            .base
            .iter()
            .filter(|t| all || EXPLORE.contains(&t.name()));
        tools.cloned().collect()
    }
    fn subagent_note(&self, member: &MemberStart) -> String {
        notebook_note(member)
    }
    /// The run's events: here only the panel's messages.
    fn on_subagent(&self, _id: &str, event: SubagentEvent) {
        let SubagentEvent::Message { from, to, text } = event else {
            return;
        };
        let (from, person) = match from {
            Sender::Agent(name) => (name, false),
            Sender::Person(name) => (name, true),
            _ => return,
        };
        let message = AgentMessage {
            from,
            to,
            text,
            person,
        };
        locked(&self.panel.messages).push(message);
    }

    /// The node starts the helper's run, or says why not.
    async fn start_member(
        &self,
        start: &MemberStart,
        _cancel: &CancellationToken,
    ) -> Result<Member, AgentError> {
        let token = {
            let mut started = locked(&self.node.started);
            let token = format!("helper-run-{}", started.len() + 1);
            started.push(Started {
                name: start.name.clone(),
                kind: start.input.subagent_type.clone(),
                asked_model: start.input.model_spec.clone(),
                token: token.clone(),
            });
            token
        };
        let model_spec = match start.input.model_spec.as_deref() {
            None => start.model_spec.clone(),
            Some("haiku") => HELPER_MODEL.into(),
            Some(other) => return Err(AgentError::Refused(format!("no model \"{other}\""))),
        };
        // Its records are its own run's, on its own clock; its calls carry its token.
        let records = Arc::new(NodeSessions::default());
        let host = Arc::new(NotebookHost::new(
            RunClock::new(T0, true),
            records.clone(),
            0,
            self.host.caps(),
        ));
        let deps = shared(&self.gateway, &token, self.workspace.clone(), host);
        Ok(Member::new(HelperRun {
            token,
            model_spec,
            deps,
            records,
            node: self.node.clone(),
        }))
    }
}

/// A helper's node run, as the hub holds it.
struct HelperRun {
    token: String,
    model_spec: String,
    deps: SharedDeps,
    records: Arc<NodeSessions>,
    node: Arc<Node>,
}

#[async_trait]
impl MemberRun for HelperRun {
    fn model_spec(&self) -> Option<String> {
        Some(self.model_spec.clone())
    }
    fn caps(&self) -> Option<MemberCaps> {
        Some(HELPER_CAPS)
    }
    fn deps(&self) -> Option<SharedDeps> {
        Some(self.deps.clone())
    }
    /// The run's records reach the node first, then the node closes it.
    async fn end(self: Box<Self>, end: MemberEnd) {
        locked(&self.node.closed).push(Closed {
            token: self.token.clone(),
            ending: end.ending,
            answer: end.answer,
            usd: end.spend.usd(),
            model_calls: end.spend.model_calls,
            records: self.records.message_times().len(),
        });
    }
}
