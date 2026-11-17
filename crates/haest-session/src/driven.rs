//! [`Driven`]: what the session driver needs from the conversation it runs,
//! and the plain implementation for a bare [`Agent`].

use async_trait::async_trait;
use haest_core::{
    Agent, AgentControl, Effort, EventSink, Message, TurnOutcome, error::BoxError, ledger::Ledger,
};

/// What the conversation says about itself between turns.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct DrivenStatus {
    /// Its session file's id; empty when it keeps none.
    pub session: String,
    /// The model, `provider:model`.
    pub model_spec: String,
    /// The effort; None = the model's own default.
    pub effort: Option<Effort>,
    /// What it has cost so far, when it keeps a ledger.
    pub usd: Option<f64>,
    /// Messages in its history.
    pub messages: usize,
}

/// PLUG POINT: what the session driver drives. The driver owns it on its own
/// task, runs one turn at a time, steers input that arrives mid-turn through
/// [`control`](Self::control), and makes changes only between turns.
///
/// Every method but [`control`](Self::control), [`run`](Self::run),
/// [`history`](Self::history), [`status`](Self::status) and the three changes
/// has a default that fits a bare agent: no gate, no inbox, nothing to close.
/// haest-runtime's `Runtime` implements all of it with its own methods (its
/// `human_turn`, `next_agent_turn`, `run` and `shut_down`), so the turn loop
/// it documents is the one the driver runs.
///
/// # Examples
///
/// A host's own conversation: an agent whose people's turns carry a note.
/// Only the required methods and the one it changes are written out.
///
/// ```
/// use haest_core::{
///     Agent, AgentControl, AgentDeps, AgentOptions, BoxError, Effort, EventSink, Message,
///     SharedDeps, ShutWorkspace, Tools, TurnOutcome,
///     testing::{ScriptedProvider, text_answer},
/// };
/// use haest_session::{Driven, DrivenStatus, Hub, Input, start};
///
/// struct Noted(Agent);
///
/// #[async_trait::async_trait]
/// impl Driven for Noted {
///     fn control(&self) -> AgentControl {
///         self.0.control()
///     }
///     async fn run(&mut self, input: &str, sink: &mut dyn EventSink) -> TurnOutcome {
///         self.0.run(input, sink).await
///     }
///     async fn human_turn(&mut self, text: &str, sink: &mut dyn EventSink) -> TurnOutcome {
///         self.0.run(&format!("(from the desk) {text}"), sink).await
///     }
///     fn history(&self) -> Vec<Message> {
///         self.0.history().to_vec()
///     }
///     fn status(&self) -> DrivenStatus {
///         Driven::status(&self.0)
///     }
///     fn set_model_spec(&mut self, spec: &str) -> Result<(), BoxError> {
///         Driven::set_model_spec(&mut self.0, spec)
///     }
///     fn set_effort(&mut self, effort: Option<Effort>) {
///         self.0.set_effort(effort);
///     }
///     async fn clear(&mut self) -> Result<(), BoxError> {
///         self.0.replace_history(Vec::new());
///         Ok(())
///     }
/// }
///
/// # #[tokio::main(flavor = "current_thread")]
/// # async fn main() -> Result<(), haest_session::SessionError> {
/// let model = ScriptedProvider::new([text_answer("noted")]);
/// let deps = AgentDeps::new(SharedDeps::new(model.resolver(), "/", ShutWorkspace), Tools::new(), "");
/// let agent = Agent::new(deps, AgentOptions::new("scripted:m"));
/// let session = start(Noted(agent), Hub::new("s1"));
///
/// session.turn(Input::human("the lamp is broken")).await?;
/// let history = session.history().await?.messages;
/// assert!(matches!(&history[0], Message::User(u) if u.content == "(from the desk) the lamp is broken"));
/// session.close().await;
/// # Ok(())
/// # }
/// ```
#[async_trait]
pub trait Driven: Send + 'static {
    /// The handle the driver steers and stops the running turn through.
    fn control(&self) -> AgentControl;

    /// One turn on `input` as it is, every event to `sink` in order; what
    /// it holds is saved when the turn ends.
    async fn run(&mut self, input: &str, sink: &mut dyn EventSink) -> TurnOutcome;

    /// A turn a person started: `text` made ready (a host's mods rewrite it,
    /// held turns ride in ahead of it), then [`run`](Self::run). Default: run
    /// as it is.
    async fn human_turn(&mut self, text: &str, sink: &mut dyn EventSink) -> TurnOutcome {
        self.run(text, sink).await
    }

    /// The input of the next turn nobody asked for, through the gate: None
    /// when nothing waits, or when the gate holds what does. Default: the
    /// control's unread steering, never held.
    fn next_agent_turn(&mut self) -> Option<String> {
        self.control().take_queued()
    }

    /// What the last turn left unread, and what arrived since, as one input,
    /// not yet through the gate. None when nothing waits. Default: the
    /// control's unread steering.
    fn take_unread(&mut self) -> Option<String> {
        self.control().take_queued()
    }

    /// The gate another program's input passes: the text to run it with, or
    /// None when it is held until a person writes. Default: never held.
    fn gate_agent(&mut self, text: String) -> Option<String> {
        Some(text)
    }

    /// Resolves when something arrives for a turn (a job ended, an agent
    /// wrote). Cancel-safe: the driver waits on it beside its requests.
    /// Default: never.
    async fn arrival(&mut self) {
        std::future::pending::<()>().await;
    }

    /// Whether work is still out that will arrive later (a background
    /// subagent's answer). Default: none.
    fn expecting(&self) -> bool {
        false
    }

    /// The messages, oldest first.
    fn history(&self) -> Vec<Message>;

    /// What it says about itself now.
    fn status(&self) -> DrivenStatus;

    /// The model from the next turn on.
    ///
    /// # Errors
    ///
    /// When it cannot use `spec`; nothing changes then.
    fn set_model_spec(&mut self, spec: &str) -> Result<(), BoxError>;

    /// The effort from the next turn on; None = the model's own default.
    fn set_effort(&mut self, effort: Option<Effort>);

    /// A fresh session: empty history.
    ///
    /// # Errors
    ///
    /// When the new session cannot be made; nothing changes then.
    async fn clear(&mut self) -> Result<(), BoxError>;

    /// The conversation ends: stop what it started, save what it holds.
    /// Default: nothing.
    async fn close(&mut self) {}
}

/// A bare agent: no session file, no gate, nothing to close. Its spend is
/// its ledger's, when it has one.
#[async_trait]
impl Driven for Agent {
    fn control(&self) -> AgentControl {
        Agent::control(self)
    }

    async fn run(&mut self, input: &str, sink: &mut dyn EventSink) -> TurnOutcome {
        Agent::run(self, input, sink).await
    }

    fn history(&self) -> Vec<Message> {
        Agent::history(self).to_vec()
    }

    fn status(&self) -> DrivenStatus {
        let usd = self.deps().shared().ledger().map(Ledger::usd);
        DrivenStatus {
            session: String::new(),
            model_spec: self.model_spec().to_string(),
            effort: self.effort(),
            usd,
            messages: Agent::history(self).len(),
        }
    }

    fn set_model_spec(&mut self, spec: &str) -> Result<(), BoxError> {
        self.deps().shared().models().resolve(spec)?;
        self.set_model_spec(spec);
        Ok(())
    }

    fn set_effort(&mut self, effort: Option<Effort>) {
        Agent::set_effort(self, effort);
    }

    async fn clear(&mut self) -> Result<(), BoxError> {
        self.replace_history(Vec::new());
        Ok(())
    }
}
