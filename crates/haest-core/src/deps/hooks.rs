//! [`AgentHooks`]: the host's say in the loop, and [`NoHooks`], which says nothing.

use async_trait::async_trait;
use tokio_util::sync::CancellationToken;

use crate::{
    agent::Agent,
    error::BoxError,
    events::TurnOutcome,
    messages::{AssistantMessage, MediaPart, Message},
    provider::Effort,
};

/// A host's say in the loop.
///
/// Set it with [`AgentDeps::with_hooks`](crate::AgentDeps::with_hooks). Every
/// method has a default that changes nothing; the loop calls them from the
/// task running [`Agent::run`], so a slow hook slows the turn. What tools
/// publish, report and log goes to
/// [`SharedDeps::services`](crate::SharedDeps::services) instead.
///
/// # Examples
///
/// A host that saves every message, with one model call per turn (a
/// [`TurnCaps`](crate::TurnCaps) as its [`TurnCapsSource`](crate::TurnCapsSource)):
///
/// ```
/// # #[cfg(feature = "testing")]
/// # #[tokio::main(flavor = "current_thread")]
/// # async fn main() -> Result<(), haest_core::BoxError> {
/// use std::sync::{Arc, Mutex};
///
/// use haest_core::{
///     Agent, AgentDeps, AgentHooks, AgentOptions, LoopEvent, Message, SharedDeps,
///     ShutWorkspace, StopReason, Tools, TurnCaps,
///     testing::{ScriptedProvider, tool_answer},
/// };
///
/// /// Clones share what it saved.
/// #[derive(Clone, Default)]
/// struct Host {
///     saved: Arc<Mutex<Vec<Message>>>,
/// }
///
/// #[async_trait::async_trait]
/// impl AgentHooks for Host {
///     fn on_message(&self, msg: &Message) {
///         self.saved.lock().expect("not poisoned").push(msg.clone());
///     }
/// }
///
/// // The model calls a tool, which needs a second model call to see the result.
/// let provider = ScriptedProvider::new([tool_answer(&[("c1", "Lookup", serde_json::json!({}))])]);
/// let host = Host::default();
/// let shared = SharedDeps::new(provider.resolver(), "/", ShutWorkspace);
/// let deps = AgentDeps::new(shared, Tools::new(), "")
///     .with_hooks(host.clone())
///     .with_turn_caps(TurnCaps { max_turn_steps: 1, ..TurnCaps::default() });
/// let mut agent = Agent::new(deps, AgentOptions::new("scripted:m"));
///
/// let end = agent.run("look it up", &mut Vec::<LoopEvent>::new()).await;
///
/// // The cap stopped the turn before the second call; the call still has its result.
/// assert_eq!(end.stop_reason, StopReason::MaxTokens);
/// assert!(matches!(&agent.history()[2], Message::Tool(_)));
/// // Every message the loop appended reached the hook.
/// assert_eq!(*host.saved.lock().expect("not poisoned"), agent.history());
/// # Ok(())
/// # }
/// # #[cfg(not(feature = "testing"))]
/// # fn main() {}
/// ```
#[async_trait]
pub trait AgentHooks: Send + Sync {
    /// Every message appended to history, in order: the persistence hook.
    ///
    /// Not called for [`Agent::replace_history`], nor for the summary
    /// [`compact`](crate::compaction::compact) puts in place: a host that
    /// persists history records those itself. Nor for a call answered as
    /// interrupted in the middle of a history a host handed in: replaying
    /// the host's file answers it the same way.
    fn on_message(&self, _msg: &Message) {}

    /// Runs before each model call; may replace history (compaction). `Err`
    /// ends the turn there, as an error with its text, without the call: the
    /// host's way out when it cannot go on (a summary that failed would
    /// otherwise be tried again before every call, each try paid). Any error
    /// will do, a `String` too (`Err(why.into())`). A turn cancelled while it
    /// ran ends as aborted, whatever it returns.
    ///
    /// `agent` is the running agent; `cancel` is its turn's token, to pass to
    /// any model call the hook makes. A model call made here through
    /// [`compact`](crate::compaction::compact) counts toward the turn's caps
    /// and goes on the conversation's ledger.
    async fn before_model_call(
        &self,
        _agent: &mut Agent,
        _cancel: &CancellationToken,
    ) -> Result<(), BoxError> {
        Ok(())
    }

    /// Runs once each answer lands, before any of its tool calls runs; for an
    /// answer without tool calls too, not for one that failed or was aborted.
    ///
    /// `answer` is the last message in `agent`'s history. The host may read the
    /// history, steer with [`Agent::control`] (the input lands after the
    /// answer's tool results, or keeps going a turn that had none), switch
    /// models with [`Agent::set_model_spec`] (from the next call), or end the
    /// turn. Leave the history as it is: the tool results are appended after
    /// the answer (compact in [`before_model_call`](Self::before_model_call)).
    ///
    /// `Err` ends the turn as an error with its text, and the answer's calls do
    /// not run: each is answered as not run, so the history can be sent again.
    /// A turn cancelled while it ran ends as aborted, its calls answered as
    /// interrupted. A model call made here through
    /// [`compact`](crate::compaction::compact) counts toward the turn's caps,
    /// which stop it before its next model call.
    ///
    /// A resumed durable run ([`Agent::run_durable`]) asks again about the
    /// answer it stopped after when none of its calls has a result (or it has
    /// none and the run's end never landed): the records do not say whether
    /// this hook returned, and the calls must not run unseen. A hook that must
    /// not see an answer twice keeps its own record.
    ///
    /// # Examples
    ///
    /// A host that lets no answer delete anything: the turn ends there, and
    /// the call never runs.
    ///
    /// ```
    /// # #[cfg(feature = "testing")]
    /// # #[tokio::main(flavor = "current_thread")]
    /// # async fn main() -> Result<(), haest_core::BoxError> {
    /// use haest_core::{
    ///     Agent, AgentDeps, AgentHooks, AgentOptions, AssistantMessage, BoxError, LoopEvent,
    ///     Message, SharedDeps, ShutWorkspace, StopReason, Tools,
    ///     testing::{ScriptedProvider, tool_answer},
    /// };
    /// use tokio_util::sync::CancellationToken;
    ///
    /// struct NoDeletes;
    ///
    /// #[async_trait::async_trait]
    /// impl AgentHooks for NoDeletes {
    ///     async fn after_model_call(
    ///         &self,
    ///         _agent: &mut Agent,
    ///         answer: &AssistantMessage,
    ///         _cancel: &CancellationToken,
    ///     ) -> Result<(), BoxError> {
    ///         match answer.tool_calls().find(|call| call.name == "Delete") {
    ///             Some(call) => Err(format!("{} is not allowed here", call.name).into()),
    ///             None => Ok(()),
    ///         }
    ///     }
    /// }
    ///
    /// let provider = ScriptedProvider::new([tool_answer(&[("c1", "Delete", serde_json::json!({}))])]);
    /// let shared = SharedDeps::new(provider.resolver(), "/", ShutWorkspace);
    /// let deps = AgentDeps::new(shared, Tools::new(), "").with_hooks(NoDeletes);
    /// let mut agent = Agent::new(deps, AgentOptions::new("scripted:m"));
    ///
    /// let end = agent.run("clean up", &mut Vec::<LoopEvent>::new()).await;
    ///
    /// assert_eq!(end.stop_reason, StopReason::Error);
    /// assert_eq!(end.error.as_deref(), Some("Delete is not allowed here"));
    /// // The call still has a result, so the history can be sent again.
    /// assert!(matches!(&agent.history()[2], Message::Tool(r) if r.is_error));
    /// # Ok(())
    /// # }
    /// # #[cfg(not(feature = "testing"))]
    /// # fn main() {}
    /// ```
    async fn after_model_call(
        &self,
        _agent: &mut Agent,
        _answer: &AssistantMessage,
        _cancel: &CancellationToken,
    ) -> Result<(), BoxError> {
        Ok(())
    }

    /// Asked when a turn is about to end on its own: an answer without tool
    /// calls, no steering queued. The stop hook: a host keeps a turn going here
    /// until its check passes (the tests pass, the file is written).
    ///
    /// `Ok(Some(text))` adds `text` as a user message, as steering does, and
    /// the loop makes another model call under the turn's caps and guard: a
    /// hook that always says `Some` ends the turn at a cap. `Ok(None)` lets it
    /// end with `outcome`; `Err` ends it as an error with its text. A turn
    /// cancelled while it ran ends as aborted. Not asked when a turn ends any
    /// other way: aborted, an error, a cap, the guard.
    ///
    /// The answer is the last message in `agent`'s history. The host may read
    /// the history and switch models with [`Agent::set_model_spec`]; input
    /// steered from here with [`Agent::control`] waits for the caller's next
    /// turn (return it to go on in this one). A model call made here through
    /// [`compact`](crate::compaction::compact) counts toward the turn's caps.
    /// A resumed durable run ([`Agent::run_durable`]) whose last answer landed
    /// without the run's end asks again: its records do not say what this said.
    ///
    /// # Examples
    ///
    /// A host that keeps the turn going until the answer says it is done, at
    /// most five model calls:
    ///
    /// ```
    /// # #[cfg(feature = "testing")]
    /// # #[tokio::main(flavor = "current_thread")]
    /// # async fn main() -> Result<(), haest_core::BoxError> {
    /// use haest_core::{
    ///     Agent, AgentDeps, AgentHooks, AgentOptions, BoxError, LoopEvent, Message, SharedDeps,
    ///     ShutWorkspace, StopReason, Tools, TurnCaps, TurnOutcome,
    ///     testing::{ScriptedProvider, text_answer},
    /// };
    /// use tokio_util::sync::CancellationToken;
    ///
    /// const GO_ON: &str = "Not done yet: go on, and say DONE when it is.";
    ///
    /// struct UntilDone;
    ///
    /// #[async_trait::async_trait]
    /// impl AgentHooks for UntilDone {
    ///     async fn before_turn_end(
    ///         &self,
    ///         agent: &mut Agent,
    ///         _outcome: &TurnOutcome,
    ///         _cancel: &CancellationToken,
    ///     ) -> Result<Option<String>, BoxError> {
    ///         let done = matches!(
    ///             agent.history().last(),
    ///             Some(Message::Assistant(answer)) if answer.text().contains("DONE")
    ///         );
    ///         Ok((!done).then(|| GO_ON.to_string()))
    ///     }
    /// }
    ///
    /// let provider = ScriptedProvider::new([text_answer("Half way."), text_answer("All DONE.")]);
    /// let shared = SharedDeps::new(provider.resolver(), "/", ShutWorkspace);
    /// let deps = AgentDeps::new(shared, Tools::new(), "")
    ///     .with_hooks(UntilDone)
    ///     .with_turn_caps(TurnCaps { max_turn_steps: 5, ..TurnCaps::default() });
    /// let mut agent = Agent::new(deps, AgentOptions::new("scripted:m"));
    ///
    /// let end = agent.run("tidy the notes", &mut Vec::<LoopEvent>::new()).await;
    ///
    /// // One turn, two model calls: the hook's text went in between.
    /// assert_eq!(end.stop_reason, StopReason::EndTurn);
    /// assert_eq!(provider.requests().len(), 2);
    /// assert!(matches!(&agent.history()[2], Message::User(u) if u.content == GO_ON));
    /// # Ok(())
    /// # }
    /// # #[cfg(not(feature = "testing"))]
    /// # fn main() {}
    /// ```
    async fn before_turn_end(
        &self,
        _agent: &mut Agent,
        _outcome: &TurnOutcome,
        _cancel: &CancellationToken,
    ) -> Result<Option<String>, BoxError> {
        Ok(None)
    }

    /// The media a user turn's text points at (attachment links), plus a note per link that failed.
    ///
    /// Called for every user message the loop adds (the turn's input,
    /// steering, and what [`before_turn_end`](Self::before_turn_end) keeps a
    /// turn going with). The notes are appended to the message's text, so the
    /// model is told what it did not get.
    async fn attach_media(
        &self,
        _text: &str,
        _cancel: &CancellationToken,
    ) -> (Vec<MediaPart>, Vec<String>) {
        (Vec::new(), Vec::new())
    }

    /// A model call withheld media the model cannot take (the human must hear it; dedupe here).
    ///
    /// Called on every model call while such media is in history, with all of
    /// it each time.
    fn on_media_withheld(&self, _parts: &[MediaPart], _model: &str) {}

    /// A model call's effort was fitted: `used` went out instead of what was asked.
    fn on_effort_fitted(&self, _model: &str, _used: Effort) {}

    /// Awaited before the loop runs a batch of tool calls that holds a tool
    /// with side effects (one that is not
    /// [`read_only`](crate::Tool::read_only)); read-only batches do not wait.
    /// Default: returns at once.
    ///
    /// A host that persists messages itself awaits its sink's
    /// [`flush`](crate::session::SessionSink::flush) here: the answer that
    /// asked for the calls (the intent) then lands before their effects, so
    /// a host that resumes after a crash knows they may have run. A durable
    /// run ([`crate::run`]) flushes its own records before this.
    async fn before_effects(&self) {}

    /// A turn's threshold warned the human
    /// ([`Action::Warn`](crate::threshold::Action::Warn)): the human must hear
    /// `text`, which ends with the host's remedy; the model does not see it.
    /// Called at the step boundary it fired at, once per firing.
    fn on_warning(&self, _text: &str) {}
}

crate::plug_ref! {
    /// An [`AgentHooks`], shared: what [`SharedDeps::hooks`](crate::SharedDeps::hooks) holds.
    pub struct AgentHooksRef(dyn AgentHooks);
}

/// Hooks that change nothing: [`AgentDeps::new`](crate::AgentDeps::new)'s default.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct NoHooks;

impl AgentHooks for NoHooks {}
