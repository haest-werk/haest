//! [`Hooks`]: hooks bound per event, as one [`Mod`](crate::Mod). Each event
//! runs its bindings in the order added (fire.rs) and folds their answers
//! by the rules a mod file's handlers are folded by too (fold.rs);
//! decisions take effect in decision.rs.

use std::fmt;

use async_trait::async_trait;
use tokio_util::sync::CancellationToken;

use crate::{
    env::ModEnv,
    events::{Event, Slots},
    fold::Handlers,
    hook::{Binding, HookError, OnError},
};

mod fire;
#[cfg(test)]
mod tests;

/// Hooks bound per event, as one [`Mod`](crate::Mod): a
/// [`Hook`](crate::Hook) is one handler for one event (a Rust closure, an
/// `async fn`, a command, a URL), and `Hooks` puts each where its event
/// fires. It goes into [`Mods`](crate::Mods) like any mod, so it runs in
/// load order among them and its failures are reported the same way.
///
/// Bind with the method of each event (`on_tool_before`, `on_tool_after`,
/// `on_prompt_submit`, `on_system_prompt`, `on_model_before`,
/// `on_model_after`, `on_turn_end`), a hook itself or a [`Binding`] with
/// its options. One event's bindings run in the order added, each seeing
/// what the ones before it left: input rewrites chain, as do output and
/// prompt rewrites, and `system.prompt` texts join. The first deny, stop or
/// continue wins, and the bindings after it are not asked. A loop event's
/// steers are all kept and its first compaction counts.
///
/// A hook that fails or runs out of time does what its binding's
/// [`OnError`] says:
///
/// | Event | `block` | `continue` |
/// |---|---|---|
/// | `tool.before` (default `block`) | the call is refused and does not run; the model reads why and the human sees it | reported, the next binding is asked |
/// | `model.before`, `model.after` | the turn stops, the hook's error its reason | as above |
/// | `tool.after`, `prompt.submit`, `system.prompt`, `turn.end` | the event fails as a mod's does ([`Mods`](crate::Mods)): what these hooks did is dropped (the output, the text as it was; nothing added; the turn ends), reported | as above (the default) |
///
/// An interrupt while a hook decides is neither: the call or turn ends as
/// interrupted, and nothing is reported.
///
/// Every payload a binding gets carries the session the conversation is on
/// as of that event, when its host named one on the [`ModEnv`]
/// ([`ModEnv::set_session`]): a hook never keeps a session of its own, so
/// after the human's /clear it reads the new one.
///
/// # Examples
///
/// A guard on Bash that keeps secrets out of the transcript, and a turn
/// that is sent back once to check its work:
///
/// ```
/// use std::sync::{
///     Arc,
///     atomic::{AtomicBool, Ordering},
/// };
///
/// use haest_core::{
///     Agent, AgentDeps, AgentOptions, LoopEvent, Message, NoHooks, SharedDeps, ShutWorkspace,
///     Tools,
///     error::BoxError,
///     testing::{ScriptedProvider, text_answer, tool_answer},
///     workspace::WorkspaceMode,
/// };
/// use haest_mods::{
///     Before, Binding, Hooks, ModEnv, ModHooks, ModTools, Mods, ToolBeforePayload,
///     TurnEndDecision, TurnEndPayload,
/// };
/// use serde_json::json;
/// use tokio_util::sync::CancellationToken;
/// # use haest_mods::{ApprovalRequest, Decision, ModUi};
/// # struct Terminal;
/// # #[async_trait::async_trait]
/// # impl ModUi for Terminal {
/// #     async fn ask(&self, _: ApprovalRequest, _: &CancellationToken) -> Decision { Decision::Deny }
/// #     fn info(&self, _: &str) {}
/// # }
/// # struct Bash;
/// # #[async_trait::async_trait]
/// # impl haest_core::Tool for Bash {
/// #     fn name(&self) -> &str { "Bash" }
/// #     fn description(&self) -> String { "Run a command".into() }
/// #     fn parameters(&self) -> serde_json::Value { json!({"type": "object"}) }
/// #     fn kind(&self) -> &str { "shell.exec" }
/// #     async fn execute(&self, _: haest_core::tool::JsonMap, _: &haest_core::ToolContext)
/// #         -> Result<haest_core::ToolOutput, haest_core::ToolError> {
/// #         Ok(haest_core::ToolOutput::text("ran"))
/// #     }
/// # }
/// # #[tokio::main(flavor = "current_thread")]
/// # async fn main() {
///
/// // tool.before, on Bash calls only: no .env in a command.
/// let guard = |p: ToolBeforePayload, _: CancellationToken| async move {
///     let command = p.call.input.get("command").and_then(|c| c.as_str()).unwrap_or("");
///     Ok::<_, BoxError>(if command.contains(".env") {
///         Before::Deny("this project keeps its secrets out of the transcript".into())
///     } else {
///         Before::Pass
///     })
/// };
///
/// // turn.end: keep the first turn going once.
/// let checked = Arc::new(AtomicBool::new(false));
/// let check_once = move |_: TurnEndPayload, _: CancellationToken| {
///     let first = !checked.swap(true, Ordering::SeqCst);
///     async move {
///         let decision = TurnEndDecision::default();
///         Ok::<_, BoxError>(if first { decision.with_continue("Check your work once more.") } else { decision })
///     }
/// };
///
/// let hooks = Hooks::new()
///     .on_tool_before(Binding::new(guard).with_matcher("Bash"))
///     .on_turn_end(check_once);
/// let mods = Mods::new(ModEnv::new("/project", WorkspaceMode::Real, Terminal), [hooks]);
///
/// let model = ScriptedProvider::new([
///     tool_answer(&[("t1", "Bash", json!({"command": "cat .env"}))]),
///     text_answer("I will not read it."),
///     text_answer("Checked: nothing else to do."),
/// ]);
/// let shared = SharedDeps::new(model.resolver(), "/project", ShutWorkspace);
/// let deps = AgentDeps::new(shared, ModTools::new(Tools::new().with(Bash), mods.clone()), "")
///     .with_hooks(ModHooks::new(NoHooks, mods));
/// let mut agent = Agent::new(deps, AgentOptions::new("scripted:m"));
/// let end = agent.run("show me the config", &mut Vec::<LoopEvent>::new()).await;
/// assert_eq!(end.error, None);
///
/// // The Bash call came back refused, and the turn went on once.
/// let refused = agent.history().iter().any(|m| {
///     matches!(m, Message::Tool(r) if r.is_error && r.content.contains("secrets"))
/// });
/// assert!(refused);
/// assert_eq!(model.requests().len(), 3);
/// # }
/// ```
#[derive(Clone)]
pub struct Hooks {
    name: String,
    pub(crate) slots: Slots,
}

impl Default for Hooks {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Debug for Hooks {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Hooks")
            .field("name", &self.name)
            .field("slots", &self.slots)
            .finish()
    }
}

impl Hooks {
    /// No hooks yet, named `hooks` in reports.
    pub fn new() -> Self {
        Self {
            name: "hooks".into(),
            slots: Slots::default(),
        }
    }

    /// The name these hooks go by in a report (`Mod <name> failed in ..`),
    /// for a host that keeps several sets (the human's, a project's).
    #[must_use]
    pub fn with_name(mut self, name: impl Into<String>) -> Self {
        self.name = name.into();
        self
    }

    /// The `nth` binding's answer to `payload`, after its [`OnError`]: an
    /// answer, `None` for a failure reported and passed over (`continue`),
    /// or the error that ends the event (`block`, or an interrupt). The
    /// one place a binding runs, so every hook gets the session the
    /// conversation is on as of this event.
    async fn ask<E: Event>(
        &self,
        binding: &Binding<E>,
        nth: usize,
        mut payload: E,
        cancel: &CancellationToken,
        haest: &ModEnv,
    ) -> Result<Option<E::Decision>, HookError> {
        payload.set_session(haest.session());
        match binding.run(payload, nth, cancel).await {
            Ok(decision) => Ok(Some(decision)),
            Err(e @ HookError::Interrupted { .. }) => Err(e),
            Err(e) => match binding.on_error() {
                OnError::Continue => {
                    haest.report_failure(&self.name, E::NAME.as_str(), &e);
                    Ok(None)
                }
                OnError::Block => Err(e),
            },
        }
    }

    /// `bindings` as the folds (fold.rs) ask them, under `cancel`.
    fn bound<'a, E: Event>(
        &'a self,
        bindings: &'a [Binding<E>],
        cancel: &'a CancellationToken,
        haest: &'a ModEnv,
    ) -> Bound<'a, E> {
        Bound {
            hooks: self,
            bindings,
            cancel,
            haest,
        }
    }
}

/// One event's bindings, as the folds ask them: a binding that is not for
/// the call is passed over, and one that fails does what its
/// [`OnError`] says.
struct Bound<'a, E: Event> {
    hooks: &'a Hooks,
    bindings: &'a [Binding<E>],
    cancel: &'a CancellationToken,
    haest: &'a ModEnv,
}

#[async_trait]
impl<E: Event> Handlers<E> for Bound<'_, E> {
    type Error = HookError;

    fn count(&self) -> usize {
        self.bindings.len()
    }

    async fn ask(&self, nth: usize, payload: E) -> Result<Option<E::Decision>, HookError> {
        let binding = &self.bindings[nth - 1];
        if !binding.takes(&payload) {
            return Ok(None);
        }
        let asked = self
            .hooks
            .ask(binding, nth, payload, self.cancel, self.haest);
        asked.await
    }
}
