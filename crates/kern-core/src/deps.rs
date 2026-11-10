//! What the loop needs (AgentDeps) and the host's optional say in it (AgentHooks).
//!
//! PLUG POINT: [`AgentHooks`] is where a host changes the loop's behaviour
//! without forking it. Every method has a default that does nothing, so a host
//! implements only what it cares about.

use std::{
    collections::HashSet,
    path::PathBuf,
    sync::{Arc, Mutex},
    time::{SystemTime, UNIX_EPOCH},
};

use async_trait::async_trait;
use tokio_util::sync::CancellationToken;

use crate::{
    agent::Agent,
    catalog::{InputModality, Reasoning},
    ledger::Ledger,
    limits::TurnLimits,
    messages::{MediaPart, Message},
    provider::{Effort, ModelResolver, WebSearchConfig},
    tool::{ToolHost, ToolSource},
    workspace::Workspace,
};

/// The system prompt, built again before every model call (instruction files
/// and skills can change mid-conversation).
#[async_trait]
pub trait SystemPrompt: Send + Sync {
    async fn system_prompt(&self) -> String;
}

#[async_trait]
impl SystemPrompt for String {
    async fn system_prompt(&self) -> String {
        self.clone()
    }
}

/// A host's say in the loop. Extends [`ToolHost`]: what tools publish, report
/// and log goes to the same object.
#[async_trait]
pub trait AgentHooks: ToolHost {
    /// Every message appended to history, in order: the persistence hook.
    fn on_message(&self, _msg: &Message) {}

    /// Runs before each model call; may replace history (compaction).
    async fn before_model_call(&self, _agent: &mut Agent, _cancel: &CancellationToken) {}

    /// Hard turn caps for a model spec, checked before every model call.
    /// None = uncapped.
    fn limits(&self, _spec: &str) -> Option<TurnLimits> {
        None
    }

    /// What a model spec takes as input; None = unknown, media sent untouched.
    fn input_modalities(&self, _spec: &str) -> Option<HashSet<InputModality>> {
        None
    }

    /// The media a user turn's text points at (attachment links), plus a note per link that failed.
    async fn attach_media(
        &self,
        _text: &str,
        _cancel: &CancellationToken,
    ) -> (Vec<MediaPart>, Vec<String>) {
        (Vec::new(), Vec::new())
    }

    /// A model call withheld media the model cannot take (the human must hear it; dedupe here).
    fn on_media_withheld(&self, _parts: &[MediaPart], _model: &str) {}

    /// A model spec's reasoning row; None = unknown, effort sent as asked.
    fn reasoning(&self, _spec: &str) -> Option<Reasoning> {
        None
    }

    /// A model call's effort was fitted: `used` went out instead of what was asked.
    fn on_effort_fitted(&self, _model: &str, _used: Effort) {}

    /// Milliseconds since the epoch, the clock the turn guard's time cap reads.
    /// A host that pauses runs returns a clock that stands still while paused.
    fn now_ms(&self) -> u64 {
        now_ms()
    }
}

/// Milliseconds since the epoch, by the wall clock.
pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

/// Hooks that change nothing.
#[derive(Debug, Default)]
pub struct NoHooks;
impl ToolHost for NoHooks {}
impl AgentHooks for NoHooks {}

/// Everything the loop needs.
#[derive(Clone)]
pub struct AgentDeps {
    pub models: Arc<dyn ModelResolver>,
    pub tools: Arc<dyn ToolSource>,
    pub system_prompt: Arc<dyn SystemPrompt>,
    pub cwd: PathBuf,
    /// Every file and shell operation a tool performs goes through this.
    pub workspace: Arc<dyn Workspace>,
    /// Where tool results too big for the context are written; None = truncate instead.
    pub spill_dir: Option<PathBuf>,
    pub hooks: Arc<dyn AgentHooks>,
    /// What the conversation has spent across turns; every model call and tool spend is charged here.
    pub ledger: Option<Arc<Mutex<Ledger>>>,
    /// Provider-side web search for every model call; None = off.
    pub web_search: Option<WebSearchConfig>,
}

impl AgentDeps {
    /// The minimum: models, tools, a system prompt and a workspace. Everything else off.
    pub fn new(
        models: Arc<dyn ModelResolver>,
        tools: Arc<dyn ToolSource>,
        system_prompt: Arc<dyn SystemPrompt>,
        cwd: impl Into<PathBuf>,
        workspace: Arc<dyn Workspace>,
    ) -> Self {
        Self {
            models,
            tools,
            system_prompt,
            cwd: cwd.into(),
            workspace,
            spill_dir: None,
            hooks: Arc::new(NoHooks),
            ledger: None,
            web_search: None,
        }
    }
}
