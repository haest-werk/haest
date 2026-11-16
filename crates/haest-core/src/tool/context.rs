//! [`ToolContext`]: what one tool call may use.

use std::{path::PathBuf, sync::Arc, time::Duration};

use tokio_util::sync::CancellationToken;

use super::{NoServices, ToolServices};
use crate::{
    fs::resolve_path,
    run::{EffectKey, RunKey},
    workspace::Workspace,
};

/// What one tool call may use: where it runs, how it learns it was cancelled,
/// the workspace and the host's services.
///
/// The loop builds one per call from [`AgentDeps`](crate::AgentDeps);
/// [`ToolContext::new`] builds one to run a tool outside the loop.
#[derive(Clone)]
pub struct ToolContext {
    /// The working directory; [`ToolContext::resolve`] takes relative paths from it.
    pub cwd: PathBuf,
    /// The turn's cancel token. It fires on abort; a tool that runs for long
    /// should stop when it does.
    pub cancel: CancellationToken,
    /// The only way a tool touches files or runs a command.
    pub workspace: Arc<dyn Workspace>,
    /// The host's services: publishing bytes, reporting inner calls, logging.
    pub services: Arc<dyn ToolServices>,
    /// Tokens a result may still add before the next model call hits the request
    /// cap; None when no cap is configured.
    pub context_room_tokens: Option<u64>,
    /// This call's id; None when a tool is run outside the loop.
    pub tool_call_id: Option<String>,
    /// Where results too big for the context are written.
    pub spill_dir: Option<PathBuf>,
    /// The durable run this call belongs to ([`crate::run`]); None outside one.
    pub run_key: Option<RunKey>,
    /// This call may have run before: a resumed durable run runs it again
    /// ([`OnResume`](super::OnResume)). False for every first run.
    pub resumed: bool,
    /// What was left of the turn's wall-clock cap
    /// ([`TurnCaps::max_turn_time`](crate::caps::TurnCaps::max_turn_time))
    /// when this step's tools started; None when the turn has no time cap or
    /// the tool runs outside the loop. The Agent tool fits a subagent's time
    /// into it.
    pub turn_time_left: Option<Duration>,
}

impl ToolContext {
    /// A context for running a tool outside the loop (tests, a host command):
    /// a fresh cancel token, [`NoServices`], no context cap, no call id, no
    /// spill directory, no durable run, no turn time cap.
    pub fn new(cwd: impl Into<PathBuf>, workspace: Arc<dyn Workspace>) -> Self {
        Self {
            cwd: cwd.into(),
            cancel: CancellationToken::new(),
            workspace,
            services: Arc::new(NoServices),
            context_room_tokens: None,
            tool_call_id: None,
            spill_dir: None,
            run_key: None,
            resumed: false,
            turn_time_left: None,
        }
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
