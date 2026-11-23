//! [`Hook`]: one handler for one event. [`Binding`]: a hook with how
//! [`Hooks`](crate::Hooks) runs it (a timeout, what its failure does, and
//! for a tool event the tools it is for), and the one place a binding runs.

use std::{fmt, future::Future, sync::Arc, time::Duration};

use async_trait::async_trait;
use haest_core::{__private::name_matches, error::BoxError};
use tokio_util::sync::CancellationToken;

use crate::events::{Event, ModEvent, ToolEvent};

/// How long a hook may take, unless its [`Binding`] says otherwise. A
/// hook that hangs must not hold a tool call or a turn for good.
pub const DEFAULT_HOOK_TIMEOUT: Duration = Duration::from_secs(60);

/// One handler for one event `E`: given the event's payload, it answers the
/// event's decision ([`Event::Decision`]), or fails.
///
/// A closure or an `async fn` taking the payload and a
/// [`CancellationToken`] is a hook; so is a type that implements this
/// (a command to run, a URL to post to). Bind one with the
/// [`Hooks`](crate::Hooks) method of its event.
///
/// ```
/// use haest_core::error::BoxError;
/// use haest_mods::{Before, Binding, Hook, ToolBeforePayload};
/// use tokio_util::sync::CancellationToken;
///
/// /// Keeps `.env` files out of the transcript.
/// async fn no_dotenv(call: ToolBeforePayload, _cancel: CancellationToken) -> Result<Before, BoxError> {
///     let command = call.call.input.get("command").and_then(|c| c.as_str()).unwrap_or("");
///     Ok(if command.contains(".env") { Before::Deny("not the secrets".into()) } else { Before::Pass })
/// }
///
/// fn bind(hook: impl Hook<ToolBeforePayload>) -> Binding<ToolBeforePayload> {
///     Binding::new(hook).with_matcher("Bash")
/// }
/// let _ = bind(no_dotenv);
/// ```
#[async_trait]
pub trait Hook<E: Event>: Send + Sync + 'static {
    /// Answer `payload`. `cancel` fires when the human interrupts the call
    /// or the turn, or the binding's time is up: stop then. An `Err` is
    /// the hook's failure; its binding's [`OnError`] says what it does.
    async fn run(&self, payload: E, cancel: &CancellationToken) -> Result<E::Decision, BoxError>;
}

#[async_trait]
impl<E, F, Fut> Hook<E> for F
where
    E: Event,
    F: Fn(E, CancellationToken) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Result<E::Decision, BoxError>> + Send,
{
    async fn run(&self, payload: E, cancel: &CancellationToken) -> Result<E::Decision, BoxError> {
        self(payload, cancel.clone()).await
    }
}

haest_core::word_enum! {
    /// What a hook's failure or timeout does: a binding's `on_error`. Its
    /// event's default is [`Event::ON_ERROR`]: `block` for `tool.before`,
    /// `continue` for every other event.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    #[non_exhaustive]
    pub enum OnError ("on_error") {
        /// Report it through [`ModUi::error`](crate::ModUi::error), and go
        /// on as if the hook had passed: the next binding is asked.
        Continue = "continue",
        /// End the event there: `tool.before` refuses the call (the model
        /// reads why, the human sees it), `model.before` and `model.after`
        /// stop the turn with the hook's error as the reason, and every
        /// other event fails as a mod's does ([`Mods`](crate::Mods)): what
        /// these hooks did is dropped and the human sees it.
        Block = "block",
    }
}

/// A [`Hook`] bound to its event `E`, with how [`Hooks`](crate::Hooks)
/// runs it: a timeout ([`DEFAULT_HOOK_TIMEOUT`] unless set), what a failure
/// does ([`OnError`], the event's default unless set) and, on a tool event,
/// which tools it is for (every tool unless set).
///
/// A hook converts into a binding with the defaults, so an `on_*` method
/// takes either:
///
/// ```
/// use std::time::Duration;
///
/// use haest_core::error::BoxError;
/// use haest_mods::{Binding, Hooks, OnError, ToolAfterPayload, TurnEndDecision, TurnEndPayload};
/// use haest_core::tool::ToolOutput;
/// use tokio_util::sync::CancellationToken;
///
/// let log = |_call: ToolAfterPayload, _: CancellationToken| async { Ok::<Option<ToolOutput>, BoxError>(None) };
/// let done = |_turn: TurnEndPayload, _: CancellationToken| async { Ok::<_, BoxError>(TurnEndDecision::default()) };
/// let hooks = Hooks::new()
///     .on_tool_after(
///         Binding::new(log)
///             .with_matcher("mcp__github__*")
///             .with_timeout(Duration::from_secs(5))
///             .with_on_error(OnError::Block),
///     )
///     .on_turn_end(done);
/// ```
pub struct Binding<E: Event> {
    hook: Arc<dyn Hook<E>>,
    timeout: Duration,
    on_error: OnError,
    matcher: Option<Matcher<E>>,
}

/// The tools a binding is for: a pattern, and how to read the tool's name
/// from the payload.
struct Matcher<E> {
    pattern: String,
    tool_name: fn(&E) -> &str,
}

impl<E> Clone for Matcher<E> {
    fn clone(&self) -> Self {
        Self {
            pattern: self.pattern.clone(),
            tool_name: self.tool_name,
        }
    }
}

impl<E: Event> Clone for Binding<E> {
    fn clone(&self) -> Self {
        Self {
            hook: self.hook.clone(),
            timeout: self.timeout,
            on_error: self.on_error,
            matcher: self.matcher.clone(),
        }
    }
}

impl<E: Event> fmt::Debug for Binding<E> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Binding")
            .field("event", &E::NAME)
            .field("timeout", &self.timeout)
            .field("on_error", &self.on_error)
            .field("matcher", &self.matcher())
            .finish_non_exhaustive()
    }
}

impl<E: Event> Binding<E> {
    /// `hook` with its event's defaults: [`DEFAULT_HOOK_TIMEOUT`],
    /// [`Event::ON_ERROR`], every tool.
    pub fn new(hook: impl Hook<E>) -> Self {
        Self {
            hook: Arc::new(hook),
            timeout: DEFAULT_HOOK_TIMEOUT,
            on_error: E::ON_ERROR,
            matcher: None,
        }
    }

    /// Give the hook up to `timeout` to answer; past it, it failed.
    #[must_use]
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// What the hook's failure or timeout does.
    #[must_use]
    pub fn with_on_error(mut self, on_error: OnError) -> Self {
        self.on_error = on_error;
        self
    }

    /// How long the hook may take.
    pub fn timeout(&self) -> Duration {
        self.timeout
    }

    /// What the hook's failure or timeout does.
    pub fn on_error(&self) -> OnError {
        self.on_error
    }

    /// The pattern of the tools this binding is for; `None` is every tool
    /// (and every call of an event that is not about one).
    pub fn matcher(&self) -> Option<&str> {
        self.matcher.as_ref().map(|m| m.pattern.as_str())
    }

    /// Whether the hook is asked about `payload`.
    pub(crate) fn takes(&self, payload: &E) -> bool {
        self.matcher
            .as_ref()
            .is_none_or(|m| name_matches((m.tool_name)(payload), &m.pattern))
    }

    /// The hook on `payload`, the `nth` (from 1) of its event's bindings,
    /// within its time: its answer, or why there is none. An interrupt
    /// (`cancel`) ends the wait at once and is never the hook's failure.
    pub(crate) async fn run(
        &self,
        payload: E,
        nth: usize,
        cancel: &CancellationToken,
    ) -> Result<E::Decision, HookError> {
        let event = E::NAME;
        // The hook's own token also fires once its time is up.
        let token = cancel.child_token();
        let _over = token.clone().drop_guard();
        let ran = tokio::select! {
            biased;
            () = cancel.cancelled() => return Err(HookError::Interrupted { event }),
            ran = tokio::time::timeout(self.timeout, self.hook.run(payload, &token)) => ran,
        };
        match ran {
            Ok(Ok(decision)) => Ok(decision),
            Ok(Err(source)) => Err(HookError::Failed { event, nth, source }),
            Err(_elapsed) => Err(HookError::TimedOut {
                event,
                nth,
                after: self.timeout,
            }),
        }
    }
}

impl<E: ToolEvent> Binding<E> {
    /// Ask the hook only about calls of the tools whose name matches
    /// `pattern`: the name itself (`Bash`), or a prefix ending in `*`
    /// (`mcp__github__*`).
    #[must_use]
    pub fn with_matcher(mut self, pattern: impl Into<String>) -> Self {
        self.matcher = Some(Matcher {
            pattern: pattern.into(),
            tool_name: E::tool_name,
        });
        self
    }
}

/// Why a bound hook gave no answer: what a failed or timed-out hook reports,
/// and the reason a blocking one refuses the call or stops the turn with.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum HookError {
    /// The hook returned an error.
    #[error("hook {nth} on {event} failed")]
    Failed {
        /// The event it was bound to.
        event: ModEvent,
        /// Which of the event's bindings it is, from 1, in the order added.
        nth: usize,
        /// What the hook returned.
        source: BoxError,
    },
    /// The hook did not answer within its binding's timeout.
    #[error("hook {nth} on {event} did not answer within {after:?}")]
    TimedOut {
        /// The event it was bound to.
        event: ModEvent,
        /// Which of the event's bindings it is, from 1, in the order added.
        nth: usize,
        /// The binding's timeout.
        after: Duration,
    },
    /// The human interrupted the call or the turn while a hook was deciding.
    #[error("interrupted during {event}")]
    Interrupted {
        /// The event.
        event: ModEvent,
    },
}
