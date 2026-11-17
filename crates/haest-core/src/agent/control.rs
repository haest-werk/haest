//! [`AgentControl`]: steering and stopping a running turn from another task.

use std::{
    fmt,
    sync::{Arc, Mutex, MutexGuard},
};

use tokio_util::sync::CancellationToken;

/// Steer or stop a running turn from another task. Cheap to clone.
///
/// Get one from [`Agent::control`](crate::Agent::control); every clone drives the same agent.
///
/// # Examples
///
/// Input that arrives while a turn runs joins that turn at its next step
/// boundary; anything still queued when it ends is the caller's next turn.
///
/// ```
/// # #[cfg(feature = "testing")]
/// # #[tokio::main(flavor = "current_thread")]
/// # async fn main() -> Result<(), haest_core::BoxError> {
/// # use haest_core::{Agent, AgentDeps, AgentOptions, SharedDeps, ShutWorkspace, Tools};
/// use haest_core::{LoopEvent, Message, testing::{ScriptedProvider, text_answer}};
///
/// let provider = ScriptedProvider::new([text_answer("one"), text_answer("two")]);
/// # let shared = SharedDeps::new(provider.resolver(), "/", ShutWorkspace);
/// # let deps = AgentDeps::new(shared, Tools::new(), "");
/// let mut agent = Agent::new(deps, AgentOptions::new("scripted:m"));
///
/// // Another task would hold this; here the sink steers once, mid-turn.
/// let control = agent.control();
/// let mut steered = false;
/// let mut sink = |ev: LoopEvent| {
///     if matches!(ev, LoopEvent::ModelStart { .. }) && !steered {
///         control.steer("also say two");
///         steered = true;
///     }
/// };
/// agent.run("say one", &mut sink).await;
///
/// // One turn, two model calls: the steering became a user message in between.
/// assert_eq!(provider.requests().len(), 2);
/// assert!(matches!(&agent.history()[2], Message::User(u) if u.content == "also say two"));
///
/// // What arrives after a turn's last step boundary waits for the caller.
/// while let Some(next) = agent.control().take_queued() {
///     agent.run(&next, &mut Vec::<LoopEvent>::new()).await;
/// }
/// # Ok(())
/// # }
/// # #[cfg(not(feature = "testing"))]
/// # fn main() {}
/// ```
#[derive(Clone)]
pub struct AgentControl {
    inner: Arc<ControlInner>,
}

#[derive(Default)]
struct ControlInner {
    queue: Mutex<Vec<String>>,
    cancel: Mutex<Option<CancellationToken>>,
}

/// Neither lock is held while anything but this file's own code runs, so a
/// poisoned one is a bug here.
impl ControlInner {
    fn queue(&self) -> MutexGuard<'_, Vec<String>> {
        self.queue
            .lock()
            .expect("agent control's queue lock poisoned")
    }

    fn cancel(&self) -> MutexGuard<'_, Option<CancellationToken>> {
        self.cancel
            .lock()
            .expect("agent control's turn lock poisoned")
    }
}

/// Whether a turn runs and how many inputs are queued; not their text.
impl fmt::Debug for AgentControl {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AgentControl")
            .field("running", &self.running())
            .field("queued", &self.inner.queue().len())
            .finish()
    }
}

impl AgentControl {
    /// A control for a new agent: nothing queued, no turn running.
    pub(super) fn new() -> Self {
        Self {
            inner: Arc::default(),
        }
    }

    /// Queue input for the running turn; it lands at the next step boundary.
    ///
    /// The boundary comes after the tool results of the current model call,
    /// or after an answer without tools, which then does not end the turn.
    /// Input queued while no turn runs waits for [`take_queued`](Self::take_queued)
    /// or the next turn's first boundary.
    pub fn steer(&self, text: impl Into<String>) {
        self.inner.queue().push(text.into());
    }

    /// Cancel the running turn, if any; does nothing between turns.
    ///
    /// The turn's cancel token fires. The model call in flight ends (a
    /// provider ends its stream as aborted), a running tool sees it on
    /// [`ToolContext::cancel`](crate::ToolContext::cancel), every call not yet
    /// started is answered as interrupted, and the turn ends with
    /// [`StopReason::Aborted`](crate::StopReason::Aborted) instead of making another model call. Every
    /// tool call keeps its result, so the history can be sent again.
    pub fn abort(&self) {
        if let Some(c) = self.inner.cancel().as_ref() {
            c.cancel();
        }
    }

    /// Whether a turn is running right now.
    pub fn running(&self) -> bool {
        self.inner.cancel().is_some()
    }

    /// Drain queued steering input, joined by blank lines; the caller runs it
    /// as the next turn. None when nothing is queued.
    pub fn take_queued(&self) -> Option<String> {
        let mut q = self.inner.queue();
        if q.is_empty() {
            return None;
        }
        Some(std::mem::take(&mut *q).join("\n\n"))
    }

    /// Mark a turn as running; Err when one already is.
    pub(crate) fn begin(&self) -> Result<CancellationToken, String> {
        let mut c = self.inner.cancel();
        if c.is_some() {
            return Err("a turn is already running; use steer()".into());
        }
        let token = CancellationToken::new();
        *c = Some(token.clone());
        Ok(token)
    }

    pub(crate) fn end(&self) {
        *self.inner.cancel() = None;
    }
}

/// Ends the running turn however `run` stops: returning, or its future dropped.
/// Anything still queued after the turn ended is a fresh turn for the caller.
pub(super) struct Running(pub(super) AgentControl);

impl Drop for Running {
    fn drop(&mut self) {
        self.0.end();
    }
}
