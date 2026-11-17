//! [`ToolContext`]: what one tool call may use.

use std::{
    path::{Path, PathBuf},
    time::Duration,
};

use tokio_util::sync::CancellationToken;

use super::{NoServices, ToolServicesRef};
use crate::{
    fs::resolve_path,
    run::{EffectKey, RunKey},
    wind_down::WindDownWatch,
    workspace::WorkspaceRef,
};

/// What one tool call may use: where it runs, how it learns it was cancelled,
/// the workspace and the host's services.
///
/// The loop builds one per call from [`AgentDeps`](crate::AgentDeps);
/// [`ToolContext::new`] builds one to run a tool outside the loop, with
/// nothing optional set, and the `with_*` setters set what a test or a host
/// command needs.
///
/// # Examples
///
/// ```
/// use std::path::Path;
///
/// use haest_core::{ShutWorkspace, ToolContext};
///
/// let ctx = ToolContext::new("/project", ShutWorkspace).with_tool_call_id("call-1");
/// assert_eq!(ctx.resolve("src/main.rs"), Path::new("/project/src/main.rs"));
/// assert_eq!(ctx.tool_call_id(), Some("call-1"));
/// assert!(!ctx.cancel().is_cancelled());
/// ```
#[derive(Clone, Debug)]
pub struct ToolContext {
    pub(crate) cwd: PathBuf,
    pub(crate) cancel: CancellationToken,
    pub(crate) workspace: WorkspaceRef,
    pub(crate) services: ToolServicesRef,
    pub(crate) context_room_tokens: Option<u64>,
    pub(crate) tool_call_id: Option<String>,
    pub(crate) spill_dir: Option<PathBuf>,
    pub(crate) run_key: Option<RunKey>,
    pub(crate) resumed: bool,
    pub(crate) turn_time_left: Option<Duration>,
    pub(crate) wind_down: WindDownWatch,
}

impl ToolContext {
    /// A context for running a tool outside the loop (tests, a host command):
    /// a fresh cancel token, [`NoServices`], no context cap, no call id, no
    /// spill directory, no durable run, no turn time cap, never winding down.
    pub fn new(cwd: impl Into<PathBuf>, workspace: impl Into<WorkspaceRef>) -> Self {
        Self {
            cwd: cwd.into(),
            cancel: CancellationToken::new(),
            workspace: workspace.into(),
            services: NoServices.into(),
            context_room_tokens: None,
            tool_call_id: None,
            spill_dir: None,
            run_key: None,
            resumed: false,
            turn_time_left: None,
            wind_down: WindDownWatch::default(),
        }
    }

    /// The working directory; [`ToolContext::resolve`] takes relative paths from it.
    pub fn cwd(&self) -> &Path {
        &self.cwd
    }

    /// The turn's cancel token. It fires on abort; a tool that runs for long
    /// should stop when it does.
    pub fn cancel(&self) -> &CancellationToken {
        &self.cancel
    }

    /// The only way a tool touches files or runs a command.
    pub fn workspace(&self) -> &WorkspaceRef {
        &self.workspace
    }

    /// The host's services: publishing bytes, reporting inner calls, logging.
    pub fn services(&self) -> &ToolServicesRef {
        &self.services
    }

    /// Tokens a result may still add before the next model call hits the
    /// request cap; None when no cap is configured.
    pub fn context_room_tokens(&self) -> Option<u64> {
        self.context_room_tokens
    }

    /// This call's id; None when a tool is run outside the loop.
    pub fn tool_call_id(&self) -> Option<&str> {
        self.tool_call_id.as_deref()
    }

    /// Where results too big for the context are written.
    pub fn spill_dir(&self) -> Option<&Path> {
        self.spill_dir.as_deref()
    }

    /// The durable run this call belongs to ([`crate::run`]); None outside one.
    pub fn run_key(&self) -> Option<&RunKey> {
        self.run_key.as_ref()
    }

    /// This call may have run before: a resumed durable run runs it again
    /// ([`OnResume`](super::OnResume)). False for every first run.
    pub fn resumed(&self) -> bool {
        self.resumed
    }

    /// What was left of the turn's wall-clock cap
    /// ([`TurnCaps::max_turn_time`](crate::caps::TurnCaps::max_turn_time))
    /// when this step's tools started; None when the turn has no time cap or
    /// the tool runs outside the loop. The Agent tool fits a subagent's time
    /// into it.
    pub fn turn_time_left(&self) -> Option<Duration> {
        self.turn_time_left
    }

    /// Whether the turn is winding down ([`wind_down`](crate::wind_down)): a
    /// tool that would start new work asks it first, and refuses while it
    /// says so. Never, unless the host turned wind-down on.
    pub fn wind_down(&self) -> &WindDownWatch {
        &self.wind_down
    }

    /// This context in `cwd`.
    pub fn with_cwd(mut self, cwd: impl Into<PathBuf>) -> Self {
        self.cwd = cwd.into();
        self
    }

    /// This context, cancelled by `cancel`.
    pub fn with_cancel(mut self, cancel: CancellationToken) -> Self {
        self.cancel = cancel;
        self
    }

    /// This context over `workspace`.
    pub fn with_workspace(mut self, workspace: impl Into<WorkspaceRef>) -> Self {
        self.workspace = workspace.into();
        self
    }

    /// This context with the host's `services`.
    pub fn with_services(mut self, services: impl Into<ToolServicesRef>) -> Self {
        self.services = services.into();
        self
    }

    /// This context with `tokens` of room before the request cap.
    pub fn with_context_room_tokens(mut self, tokens: u64) -> Self {
        self.context_room_tokens = Some(tokens);
        self
    }

    /// This context for the call `id`.
    pub fn with_tool_call_id(mut self, id: impl Into<String>) -> Self {
        self.tool_call_id = Some(id.into());
        self
    }

    /// This context, spilling big results into `dir`.
    pub fn with_spill_dir(mut self, dir: impl Into<PathBuf>) -> Self {
        self.spill_dir = Some(dir.into());
        self
    }

    /// This context inside the durable run `key`.
    pub fn with_run_key(mut self, key: RunKey) -> Self {
        self.run_key = Some(key);
        self
    }

    /// This context for a call that may have run before ([`Self::resumed`]).
    pub fn with_resumed(mut self, resumed: bool) -> Self {
        self.resumed = resumed;
        self
    }

    /// This context with `left` of the turn's time cap.
    pub fn with_turn_time_left(mut self, left: Duration) -> Self {
        self.turn_time_left = Some(left);
        self
    }

    /// This context, winding down as `watch` says.
    pub fn with_wind_down(mut self, watch: WindDownWatch) -> Self {
        self.wind_down = watch;
        self
    }

    /// A path the model gave, made absolute: a relative one is taken from
    /// `cwd`, and `.` / `..` are resolved without touching any file system.
    pub fn resolve(&self, p: &str) -> PathBuf {
        resolve_path(&self.cwd, p)
    }

    /// This call's effect key ([`RunKey::effect_key`]): the same every time
    /// the run runs this call, resumed or not. Hand it to an outside API as
    /// its idempotency key. None outside a durable run, or with no call id.
    pub fn effect_key(&self) -> Option<EffectKey> {
        let id = self.tool_call_id.as_deref()?;
        Some(self.run_key.as_ref()?.effect_key(id))
    }
}

#[cfg(test)]
mod tests;
