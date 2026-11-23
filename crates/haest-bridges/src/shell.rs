//! [`ShellHook`]: a hook that is a command, run on the host once per event,
//! the event on its stdin and the decision on its stdout.

use std::{
    path::{Path, PathBuf},
    time::Duration,
};

use async_trait::async_trait;
use haest_core::{ChildEnv, error::BoxError, workspace::ExecOptions};
use haest_mods::{Event, Hook};
use haest_workspace::RealWorkspace;
use tokio_util::sync::CancellationToken;

use crate::dialect::{Dialect, Haest, HookFailure, Reply, outgoing, shown};

#[cfg(test)]
mod tests;

/// A command has no deadline of its own: its binding's timeout drops the
/// run, and a dropped run kills the command's whole process group
/// (haest-workspace's real exec), as an interrupt does.
const NO_DEADLINE: Duration = Duration::MAX;

/// A hook that is a command: run once per event on the host, the event as
/// JSON on its stdin, the decision read from its stdout. Bind it as any
/// [`Hook`](haest_mods::Hook), with the timeout, `on_error` and tool
/// matcher of its [`Binding`](haest_mods::Binding).
///
/// It speaks a dialect: [`Haest`] (the default: the payload's own JSON in,
/// the decision's own JSON out, every event) or [`Claude`](crate::Claude)
/// (Claude Code's hook JSON, so its hook scripts run as they are;
/// [`with_dialect`](Self::with_dialect)). A `ShellHook<Claude>` is a hook
/// only of the events Claude Code has, `tool.before`, `tool.after`,
/// `prompt.submit`, `system.prompt` and `turn.end`, as
/// [`ModEvent::claude_code`](haest_mods::ModEvent::claude_code) lists them:
/// bound to `model.before` or `model.after` it does not compile. Neither
/// dialect holds anything of the conversation: each event's payload brings
/// it.
///
/// # How a command answers
///
/// | Exit | stdout | What it is |
/// |---|---|---|
/// | 0 | empty | a pass: the event goes on as it was |
/// | 0 | the event's decision, in the dialect | that decision |
/// | 0 | anything else | a failure |
/// | 2 | (ignored) | the event's blocking answer, stderr its text (cut in the middle past [`TRUNCATE_CHARS`](haest_core::tool::TRUNCATE_CHARS), as a tool's output is): `tool.before` refuses the call, `model.before` and `model.after` stop the turn, `turn.end` keeps it going; on any other event, a failure |
/// | any other | (ignored) | a failure, stderr in its report |
///
/// A failure, a timeout and a command that cannot start all do what the
/// binding's [`OnError`](haest_mods::OnError) says. haest's defaults fail
/// closed: a `tool.before` hook that breaks refuses the call (Claude Code's
/// default lets it through), every other event goes on and the human sees
/// the failure.
///
/// # Where it runs
///
/// On the host, never in the agent's sandbox, through the same exec as the
/// real workspace's commands ([`RealWorkspace::shell_with_stdin`]): `bash
/// -c <command>` in a process group of its own, killed whole when its
/// binding's time is up or the human interrupts the call or the turn. It
/// runs in the conversation's directory: [`with_cwd`](Self::with_cwd), else
/// a tool event's own (`ctx.cwd`), else the host process's working
/// directory. It gets the [`ChildEnv`] the host hands in
/// ([`with_env`](Self::with_env)) and nothing else of the host's
/// environment, as every process haest starts (a Claude Code hook inherits
/// all of it: a host that runs those scripts hands in what they need), plus
/// what its dialect adds (`CLAUDE_PROJECT_DIR` for [`Claude`](crate::Claude)).
/// The first 64 MiB of each of stdout and stderr are kept.
///
/// A hook runs with the host's full trust: load a project's hooks only from
/// a directory the human trusts.
///
/// # Examples
///
/// A guard on Bash that refuses `rm -rf`, in haest's dialect: the call's
/// JSON is on stdin, and `{ "deny": .. }` refuses it.
///
/// ```
/// # use async_trait::async_trait;
/// # use haest_core::{ToolContext, ToolOutput, ToolError, ShutWorkspace, tool::{JsonMap, Tool}};
/// # use haest_mods::{ApprovalRequest, Decision, ModUi};
/// # struct Terminal;
/// # #[async_trait]
/// # impl ModUi for Terminal {
/// #     async fn ask(&self, _: ApprovalRequest, _: &tokio_util::sync::CancellationToken) -> Decision { Decision::Deny }
/// #     fn info(&self, _: &str) {}
/// # }
/// # struct Bash;
/// # #[async_trait]
/// # impl Tool for Bash {
/// #     fn name(&self) -> &str { "Bash" }
/// #     fn description(&self) -> String { "Run a command".into() }
/// #     fn parameters(&self) -> serde_json::Value { serde_json::json!({"type": "object"}) }
/// #     fn kind(&self) -> &str { "shell.exec" }
/// #     async fn execute(&self, _: JsonMap, _: &ToolContext) -> Result<ToolOutput, ToolError> {
/// #         Ok(ToolOutput::text("ran"))
/// #     }
/// # }
/// # #[tokio::main(flavor = "current_thread")]
/// # async fn main() -> Result<(), haest_core::error::BoxError> {
/// use haest_bridges::ShellHook;
/// use haest_core::{Tools, tool::input, workspace::WorkspaceMode};
/// use haest_mods::{Binding, HookSet, ModEnv, ModTools, Mods, ToolBeforePayload};
/// use serde_json::json;
///
/// let guard = ShellHook::new(r#"grep -q 'rm -rf' && echo '{"deny": "not on this machine"}' || true"#);
/// let hooks = HookSet::new().on_tool_before(Binding::<ToolBeforePayload>::new(guard).with_matcher("Bash"));
///
/// let project = std::env::temp_dir();
/// let mods = Mods::new(ModEnv::new(&project, WorkspaceMode::Real, Terminal), [hooks]);
/// let tools = ModTools::wrap(&mods, Tools::new().with(Bash));
/// let ctx = ToolContext::new(&project, ShutWorkspace);
///
/// let refused = tools[0].execute(input(json!({ "command": "rm -rf ~" }))?, &ctx).await;
/// assert!(refused.is_err_and(|e| e.to_string().contains("not on this machine")));
/// tools[0].execute(input(json!({ "command": "ls" }))?, &ctx).await?;
/// # Ok(())
/// # }
/// ```
///
/// A Claude Code hook script, as it is, on `tool.before` and `turn.end`.
/// The session it is told (`session_id`, `transcript_path`) is the one the
/// conversation's [`ModEnv`](haest_mods::ModEnv) names at each event: the
/// host names the new one at the human's /clear, and the hooks stay bound.
/// That is the [`HookSet`](haest_mods::HookSet)'s doing: a host that calls
/// [`Hook::run`](haest_mods::Hook::run) itself, in its own `Mod`, sets the
/// payload's `session` first (from `haest.session()`).
///
/// ```
/// use haest_bridges::{Claude, ShellHook};
/// use haest_core::workspace::WorkspaceMode;
/// use haest_mods::{Binding, HookSet, ModEnv, Mods, SessionView, ToolBeforePayload};
/// # use haest_mods::{ApprovalRequest, Decision, ModUi};
/// # struct Terminal;
/// # #[async_trait::async_trait]
/// # impl ModUi for Terminal {
/// #     async fn ask(&self, _: ApprovalRequest, _: &tokio_util::sync::CancellationToken) -> Decision { Decision::Deny }
/// #     fn info(&self, _: &str) {}
/// # }
///
/// let hooks = HookSet::new()
///     .on_tool_before(
///         Binding::<ToolBeforePayload>::new(
///             ShellHook::new("python3 .claude/hooks/guard.py").with_dialect(Claude),
///         )
///         .with_matcher("Bash"),
///     )
///     .on_turn_end(ShellHook::new(".claude/hooks/tests-pass.sh").with_dialect(Claude));
/// let env = ModEnv::new("/project", WorkspaceMode::Real, Terminal)
///     .with_session(SessionView::new("conversation-7"));
/// let mods = Mods::new(env, [hooks]);
///
/// // The human's /clear: the hooks are told the new session from the next event on.
/// mods.env().set_session(SessionView::new("conversation-8"));
/// ```
///
/// Claude Code has no `model.before`: a `Claude` hook there does not compile.
///
/// ```compile_fail
/// use haest_bridges::{Claude, ShellHook};
/// use haest_mods::HookSet;
///
/// let hooks = HookSet::new().on_model_before(ShellHook::new("./x.sh").with_dialect(Claude));
/// ```
#[derive(Debug, Clone)]
pub struct ShellHook<D = Haest> {
    command: String,
    dialect: D,
    cwd: Option<PathBuf>,
    env: ChildEnv,
}

impl ShellHook {
    /// `command` (run as `bash -c <command>`), in haest's dialect, with no
    /// variables of the host's: [`with_env`](Self::with_env) hands some in.
    pub fn new(command: impl Into<String>) -> Self {
        Self {
            command: command.into(),
            dialect: Haest,
            cwd: None,
            env: ChildEnv::default(),
        }
    }
}

impl<D> ShellHook<D> {
    /// The same command, speaking `dialect`: [`Haest`], or
    /// [`Claude`](crate::Claude) for Claude Code's hook scripts.
    pub fn with_dialect<T>(self, dialect: T) -> ShellHook<T> {
        ShellHook {
            command: self.command,
            dialect,
            cwd: self.cwd,
            env: self.env,
        }
    }

    /// Run in `cwd`, the conversation's directory, whatever the event.
    #[must_use]
    pub fn with_cwd(mut self, cwd: impl Into<PathBuf>) -> Self {
        self.cwd = Some(cwd.into());
        self
    }

    /// Hand the command `env` (its `PATH`, its `HOME`, a key it needs), and
    /// nothing else of the host's environment.
    #[must_use]
    pub fn with_env(mut self, env: ChildEnv) -> Self {
        self.env = env;
        self
    }

    /// The command, as `bash -c` runs it.
    pub fn command(&self) -> &str {
        &self.command
    }

    /// The directory it runs in when the host set one.
    pub fn cwd(&self) -> Option<&Path> {
        self.cwd.as_deref()
    }

    /// The dialect it speaks.
    pub fn dialect(&self) -> &D {
        &self.dialect
    }

    /// `payload` through the command: its decision, or why there is none.
    async fn ask<E: Event>(
        &self,
        payload: E,
        cancel: &CancellationToken,
    ) -> Result<E::Decision, HookFailure>
    where
        D: Dialect<E>,
    {
        let out = outgoing(&self.dialect, &payload, self.cwd.as_deref())?;
        let env = self.env.overlaid(&self.dialect.env(&out.dir));
        let opts = ExecOptions::new(&out.dir, NO_DEADLINE).with_cancel(cancel.clone());
        let ran = RealWorkspace::new(env)
            .shell_with_stdin(&self.command, out.body, opts)
            .await;
        if cancel.is_cancelled() {
            return Err(HookFailure::Interrupted);
        }
        let reply = match ran.exit_code {
            0 => Reply::Answer(ran.stdout),
            2 => Reply::Blocked(ran.stderr),
            code => {
                return Err(HookFailure::Exit {
                    code,
                    stderr: shown(&ran.stderr),
                });
            }
        };
        self.dialect.decide(payload, reply)
    }
}

/// A hook of every event its dialect speaks: every one in [`Haest`], the
/// ones Claude Code has in [`Claude`](crate::Claude).
#[async_trait]
impl<E: Event, D: Dialect<E>> Hook<E> for ShellHook<D> {
    async fn run(&self, payload: E, cancel: &CancellationToken) -> Result<E::Decision, BoxError> {
        Ok(self.ask(payload, cancel).await?)
    }
}
