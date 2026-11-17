//! What the loop needs ([`AgentDeps`]), the host's optional say in it
//! ([`AgentHooks`]), and the clock its time cap reads ([`Clock`]).
//!
//! [`SharedDeps::new`] takes what every agent of a conversation runs on: a
//! [`ModelResolver`](crate::ModelResolver), a working directory and a
//! [`Workspace`](crate::Workspace). [`AgentDeps::new`] takes that and the
//! agent's own [`ToolSource`](crate::ToolSource) and [`SystemPrompt`], each
//! as a plain value. Everything else is off until the host sets it with a
//! `with_*` setter ([`AgentDeps::with_hooks`], [`SharedDeps::with_ledger`],
//! ...). [`Agent::new`](crate::Agent::new) takes the result.
//!
//! Every plug point is held by a handle named after its trait
//! ([`AgentHooksRef`], [`SystemPromptRef`], ...): cheap to clone, and built
//! from the value itself, or from an `Arc` the host already shares, so one
//! object can fill several slots.
//!
//! PLUG POINT: [`AgentHooks`] is where a host changes the loop's behaviour
//! without forking it. Every method has a default that does nothing, so a host
//! implements only what it cares about. [`NoHooks`] is the one that changes
//! nothing.

mod agent_deps;
mod shared;

use std::time::{SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use tokio_util::sync::CancellationToken;

pub use agent_deps::AgentDeps;
pub use shared::SharedDeps;

use crate::{
    agent::Agent,
    error::BoxError,
    messages::{MediaPart, Message},
    provider::Effort,
};

/// The system prompt, built again before every model call (instruction files
/// and skills can change mid-conversation).
///
/// A `String` or a `&'static str` is a fixed prompt, and a closure
/// `Fn() -> String` builds one each call. A host that assembles one from
/// files and skills implements this, typically with
/// [`build_system_prompt`](crate::context::build_system_prompt).
#[async_trait]
pub trait SystemPrompt: Send + Sync {
    /// The prompt for the next model call, and for compaction's summary
    /// request (which must send the same one to hit the prompt cache).
    async fn system_prompt(&self) -> String;
}

/// A fixed prompt: the string itself, every call.
#[async_trait]
impl SystemPrompt for String {
    async fn system_prompt(&self) -> String {
        self.clone()
    }
}

/// A fixed prompt: the string itself, every call.
#[async_trait]
impl SystemPrompt for &'static str {
    async fn system_prompt(&self) -> String {
        (*self).to_owned()
    }
}

/// A prompt the closure builds, every call.
#[async_trait]
impl<F: Fn() -> String + Send + Sync> SystemPrompt for F {
    async fn system_prompt(&self) -> String {
        self()
    }
}

crate::plug_ref! {
    /// A [`SystemPrompt`], shared: what [`AgentDeps`] holds. Built from the
    /// prompt itself (`"You are terse."`, a `String`, a closure, a host's
    /// own type), or from an `Arc` of one.
    pub struct SystemPromptRef(dyn SystemPrompt);
}

/// A host's say in the loop.
///
/// Set it with [`AgentDeps::with_hooks`]. Every method has a default that
/// changes nothing; the loop calls them from the task running
/// [`Agent::run`], so a slow hook slows the turn. What tools publish, report
/// and log goes to [`SharedDeps::services`] instead.
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
    /// persists history records those itself.
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
    /// [`compact`](crate::compaction::compact) counts toward the turn's caps.
    async fn before_model_call(
        &self,
        _agent: &mut Agent,
        _cancel: &CancellationToken,
    ) -> Result<(), BoxError> {
        Ok(())
    }

    /// The media a user turn's text points at (attachment links), plus a note per link that failed.
    ///
    /// Called for every user message the loop adds (the turn's input and
    /// steering). The notes are appended to the message's text, so the model
    /// is told what it did not get.
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
    /// An [`AgentHooks`], shared: what [`SharedDeps::hooks`] holds.
    pub struct AgentHooksRef(dyn AgentHooks);
}

/// Milliseconds since the Unix epoch, by the wall clock; 0 if the clock is
/// set before 1970. What [`WallClock`] reads.
pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

/// PLUG POINT: the clock the turn guard's time cap reads, in milliseconds
/// since the epoch ([`SharedDeps::with_clock`]). A host that pauses runs
/// gives one that stands still while paused. A closure `Fn() -> u64` is a
/// clock.
pub trait Clock: Send + Sync {
    /// Now, in milliseconds since the epoch. Default: the wall clock
    /// ([`now_ms`]), so a host object that fills every slot
    /// ([`SharedDeps::with_host`]) keeps it with an empty `impl`.
    fn now_ms(&self) -> u64 {
        now_ms()
    }
}

impl<F: Fn() -> u64 + Send + Sync> Clock for F {
    fn now_ms(&self) -> u64 {
        self()
    }
}

crate::plug_ref! {
    /// A [`Clock`], shared: what [`SharedDeps::clock`] holds.
    pub struct ClockRef(dyn Clock);
}

/// The wall clock ([`now_ms`]): [`AgentDeps::new`]'s default.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct WallClock;

impl Clock for WallClock {
    fn now_ms(&self) -> u64 {
        now_ms()
    }
}

/// Hooks that change nothing: [`AgentDeps::new`]'s default.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct NoHooks;

impl AgentHooks for NoHooks {}

#[cfg(test)]
mod tests;
