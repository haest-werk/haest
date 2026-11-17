//! The conversation behind a session (feature `session`): [`Runtime`] as
//! haest-session's [`Driven`], and [`HubUi`], a [`Ui`] that tells the
//! session's stream instead of a terminal.
//!
//! The session's driver runs the runtime's own turn loop: a person's text
//! through the mods' `prompt.submit` and [`Runtime::human_turn`]'s gate
//! (steered in, when a turn runs), each turn the inbox holds through
//! [`Runtime::next_agent_turn`] and [`Runtime::run`] (which steers what
//! reaches the inbox into the running turn and flushes the session file),
//! and [`Runtime::shut_down`] at the end.

use async_trait::async_trait;
use haest_session::{Driven, DrivenStatus, Hub, RewritePrompt, RewritePromptRef};
use haest_wire::{Level, Outbound, Path};

use crate::{
    Runtime, Ui,
    haest::{
        Agent, AgentControl, Effort, EventSink, Message, TurnOutcome, core::error::BoxError,
        mods::Mods, tools::SubagentEvent,
    },
};

/// The conversation's spend so far, in US dollars, from `agent`'s ledger.
/// None when the agent has no ledger (one [`build`](crate::build) made always
/// has). A resumed session's spend includes what it cost before.
fn session_usd(agent: &Agent) -> Option<f64> {
    let ledger = agent.deps().shared().ledger()?;
    Some(ledger.usd())
}

/// The mods' `prompt.submit`, as the session driver's rewriter.
struct ModsPrompt(Mods);

#[async_trait]
impl RewritePrompt for ModsPrompt {
    async fn rewrite_prompt(&self, text: &str) -> String {
        self.0.rewrite_prompt(text).await
    }
}

/// The whole conversation as a session drives it. Each turn step is one of
/// the runtime's own methods, the ones a host without a session calls
/// ([`Runtime::human_turn`], [`Runtime::next_agent_turn`], [`Runtime::run`],
/// [`Runtime::shut_down`]): there is one turn loop, and the driver runs it.
/// The driver runs a person's text through the mods itself, so input
/// steered into a running turn is rewritten too.
#[async_trait]
impl<U: Ui + 'static> Driven for Runtime<U> {
    fn control(&self) -> AgentControl {
        self.agent().control()
    }

    /// [`Runtime::run`]: what reaches the inbox meanwhile is steered in, and
    /// the session file is flushed when it ends.
    async fn run(&mut self, input: &str, sink: &mut dyn EventSink) -> TurnOutcome {
        Runtime::run(self, input, sink).await
    }

    /// The mods' `prompt.submit` ([`Mods::rewrite_prompt`]), on a clone of
    /// the runtime's [`Mods`].
    fn rewriter(&self) -> Option<RewritePromptRef> {
        Some(ModsPrompt(self.mods.clone()).into())
    }

    /// [`Runtime::human_turn`] after its rewrite (the driver ran the
    /// [`rewriter`](Driven::rewriter)): what the gate held riding in ahead of
    /// the text, then [`Runtime::run`].
    async fn human_turn(&mut self, text: &str, sink: &mut dyn EventSink) -> TurnOutcome {
        let text = self.inbox.human(text);
        Runtime::run(self, &text, sink).await
    }

    /// [`Runtime::next_agent_turn`].
    fn next_agent_turn(&mut self) -> Option<String> {
        Runtime::next_agent_turn(self)
    }

    /// What the last turn left unread, and what reached the inbox since.
    fn take_unread(&mut self) -> Option<String> {
        let control = self.agent().control();
        self.inbox.take(&control)
    }

    /// The inbox's gate (the `max_agent_turns` setting).
    fn gate_agent(&mut self, text: String) -> Option<String> {
        self.inbox.agent_turn(&text)
    }

    /// The next arrival in the inbox; never, once nothing can send one.
    async fn arrival(&mut self) {
        if !self.inbox.wait().await {
            // Every sender is gone: nothing will arrive.
            std::future::pending::<()>().await;
        }
    }

    /// A background subagent's answer is still due.
    fn expecting(&self) -> bool {
        self.subagents.answers_due() > 0
    }

    fn history(&self) -> Vec<Message> {
        self.agent().history().to_vec()
    }

    fn status(&self) -> DrivenStatus {
        DrivenStatus {
            session: self.session_id(),
            model_spec: self.agent().model_spec().to_string(),
            effort: self.agent().effort(),
            usd: session_usd(self.agent()),
            messages: self.agent().history().len(),
        }
    }

    fn set_model_spec(&mut self, spec: &str) -> Result<(), BoxError> {
        Ok(Runtime::set_model_spec(self, spec)?)
    }

    fn set_effort(&mut self, effort: Option<Effort>) {
        Runtime::set_effort(self, effort);
    }

    async fn clear(&mut self) -> Result<(), BoxError> {
        Ok(Runtime::clear(self).await?)
    }

    /// [`Runtime::shut_down`]: nothing it started outlives it, and the
    /// session is on disk. A host that tells the human what stopped calls
    /// it first (through the session's `with`); this finds nothing left.
    async fn close(&mut self) {
        self.shut_down().await;
    }
}

/// A [`Ui`] that tells the session's stream: every line a `notice` frame, an
/// arrival one too, and a subagent's life as frames under its path
/// ([`subagent_frame`]). For a host with no terminal of its own (a child
/// driven over stdio, a service); a host with one publishes the same way
/// from its own `Ui` and renders the stream.
///
/// Make it from the [`Hub`] before the build, so what the build says is in
/// the stream.
///
/// # Examples
///
/// A conversation over a host's own files, driven through a session: the
/// [`Hub`] first, so what the build says is already in the stream; the
/// approvals put to whoever drives it. Nothing here calls a model.
///
/// ```
/// use haest_core::fs::MemFs;
/// use haest_runtime::{BuildOptions, HubUi, RuntimeInputs, World, build};
/// use haest_session::{Body, Hub, SessionPrompter, start};
/// use haest_settings::{BuiltinTools, Settings};
/// use haest_wire::{Outbound, State};
///
/// # #[tokio::main(flavor = "current_thread")]
/// # async fn main() -> Result<(), haest_core::error::BoxError> {
/// let hub = Hub::new("run-1");
/// let mut watcher = hub.subscribe(0);
///
/// let settings = Settings {
///     tools: BuiltinTools { read: true, ..BuiltinTools::NONE },
///     data_dir: "/data".into(),
///     ..Settings::bare("/work", "/home/me")
/// };
/// let inputs = RuntimeInputs::new(settings, World::files(MemFs::new()))
///     .with_keys(|name: &str| (name == "OPENROUTER_API_KEY").then(|| "sk-or-example".to_string()));
/// let ui = HubUi::new(hub.clone());
/// let rt = build(inputs, ui, SessionPrompter::new(hub.clone()), BuildOptions::default()).await?;
/// let id = rt.session_id();
///
/// let session = start(rt, hub);
/// let now = session.snapshot().await?;
/// assert_eq!((now.state, now.session.as_str()), (State::Idle, id.as_str()));
///
/// // The watcher saw the session start; closing it shuts the conversation down.
/// let first = watcher.recv().await.ok_or("the stream ended")?;
/// assert!(matches!(&first.body, Body::Wire(Outbound::Status(_))));
/// session.close().await;
/// # Ok(())
/// # }
/// ```
#[derive(Debug, Clone)]
pub struct HubUi {
    hub: Hub,
}

impl HubUi {
    /// Lines into `hub`.
    pub fn new(hub: Hub) -> Self {
        Self { hub }
    }

    /// The stream the lines go into: a host's own `Ui` over this one
    /// publishes its other frames there.
    pub fn hub(&self) -> &Hub {
        &self.hub
    }
}

impl Ui for HubUi {
    fn info(&self, text: &str) {
        self.hub.notice(Level::Info, text);
    }

    fn warn(&self, text: &str) {
        self.hub.notice(Level::Warning, text);
    }

    fn error(&self, text: &str) {
        self.hub.notice(Level::Error, text);
    }

    fn subagent(&self, id: &str, event: &SubagentEvent) {
        subagent_frame(&self.hub, id, event);
    }
}

/// A subagent's event as the stream carries it, under `["main", id]`:
/// `subagent_started`, each of its loop events, `subagent_stopped`. Returns
/// the frame's seq; None for what the wire has no frame for yet (a Code
/// tool's inner calls).
///
/// ```
/// use haest_core::LoopEvent;
/// use haest_runtime::subagent_frame;
/// use haest_session::{Body, Hub};
/// use haest_tools::SubagentEvent;
/// use haest_wire::{Outbound, Path};
///
/// # #[tokio::main(flavor = "current_thread")]
/// # async fn main() -> Result<(), haest_core::error::BoxError> {
/// let hub = Hub::new("s1");
/// let mut sub = hub.subscribe(0);
/// let started = SubagentEvent::Started {
///     name: "reader".into(),
///     parent: "main".into(),
///     description: "read the docs".into(),
/// };
/// subagent_frame(&hub, "a1", &started);
/// subagent_frame(&hub, "a1", &SubagentEvent::Loop(LoopEvent::TextDelta { text: "hi".into() }));
///
/// let first = sub.recv().await.ok_or("the stream ended")?;
/// assert_eq!(first.path, Path::subagent("a1"));
/// assert!(matches!(&first.body, Body::Wire(Outbound::SubagentStarted { name, .. }) if name == "reader"));
/// let next = sub.recv().await.ok_or("the stream ended")?;
/// assert!(matches!(next.body, Body::Loop(LoopEvent::TextDelta { .. })));
/// # Ok(())
/// # }
/// ```
pub fn subagent_frame(hub: &Hub, id: &str, event: &SubagentEvent) -> Option<u64> {
    let path = Path::subagent(id);
    let seq = match event {
        SubagentEvent::Started {
            name,
            parent,
            description,
        } => {
            let frame = Outbound::SubagentStarted {
                name: name.clone(),
                parent: parent.clone(),
                description: description.clone(),
            };
            hub.publish(path, frame)
        }
        SubagentEvent::Loop(ev) => hub.publish(path, ev.clone()),
        SubagentEvent::Stopped { ok } => hub.publish(path, Outbound::SubagentStopped { ok: *ok }),
        // Inner calls, messages between agents and any later kind stay off the wire.
        _ => return None,
    };
    Some(seq)
}
