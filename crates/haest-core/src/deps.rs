//! What the loop needs ([`AgentDeps`]), the host's optional say in it
//! ([`AgentHooks`]), and the clock its time cap reads ([`Clock`]).
//!
//! [`AgentDeps::new`] takes the four things every agent needs (a
//! [`ModelResolver`], a [`ToolSource`], a [`SystemPrompt`] and a
//! [`Workspace`]); everything else is a public field of its [`SharedDeps`],
//! off until the host sets it (`deps.shared.hooks = ...`). [`Agent::new`]
//! takes the result.
//!
//! PLUG POINT: [`AgentHooks`] is where a host changes the loop's behaviour
//! without forking it. Every method has a default that does nothing, so a host
//! implements only what it cares about. [`NoHooks`] is the one that changes
//! nothing.

use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
    time::{SystemTime, UNIX_EPOCH},
};

use async_trait::async_trait;
use tokio_util::sync::CancellationToken;

use crate::{
    agent::Agent,
    caps::{TurnCaps, TurnCapsSource},
    catalog::{ModelFacts, NoFacts},
    ledger::Ledger,
    messages::{MediaPart, Message},
    provider::{Effort, ModelResolver, WebSearchOptions},
    remedy::Remedies,
    run::RunStore,
    tool::{ToolServices, ToolSource},
    workspace::Workspace,
};

/// The system prompt, built again before every model call (instruction files
/// and skills can change mid-conversation).
///
/// A `String` is a fixed prompt. A host that assembles one from files and
/// skills implements this, typically with
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

/// A host's say in the loop. Extends [`ToolServices`]: what tools publish, report
/// and log goes to the same object.
///
/// Set it on [`SharedDeps::hooks`]. Every method has a default that changes
/// nothing; the loop calls them from the task running [`Agent::run`], so a
/// slow hook slows the turn.
///
/// # Examples
///
/// A host that saves every message, with one model call per turn (a
/// [`TurnCaps`] as its [`TurnCapsSource`]):
///
/// ```
/// # #[cfg(feature = "testing")]
/// # #[tokio::main(flavor = "current_thread")]
/// # async fn main() {
/// use std::sync::{Arc, Mutex};
///
/// use haest_core::{
///     Agent, AgentDeps, AgentHooks, AgentOptions, LoopEvent, Message, ShutWorkspace,
///     StopReason, Tool, ToolServices, TurnCaps,
///     testing::{ScriptedProvider, tool_answer},
/// };
///
/// #[derive(Default)]
/// struct Host {
///     saved: Mutex<Vec<Message>>,
/// }
///
/// impl ToolServices for Host {}
///
/// #[async_trait::async_trait]
/// impl AgentHooks for Host {
///     fn on_message(&self, msg: &Message) {
///         self.saved.lock().unwrap().push(msg.clone());
///     }
/// }
///
/// // The model calls a tool, which needs a second model call to see the result.
/// let provider = ScriptedProvider::new([tool_answer(&[("c1", "Lookup", serde_json::json!({}))])]);
/// let host = Arc::new(Host::default());
/// let tools: Vec<Arc<dyn Tool>> = Vec::new();
/// let mut deps = AgentDeps::new(
///     Arc::new(provider.resolver()),
///     Arc::new(tools),
///     Arc::new(String::new()),
///     "/",
///     Arc::new(ShutWorkspace),
/// );
/// deps.shared.hooks = host.clone();
/// deps.shared.turn_caps = Arc::new(TurnCaps { max_turn_steps: 1, ..TurnCaps::default() });
/// let mut agent = Agent::new(deps, AgentOptions::new("scripted:m"));
///
/// let end = agent.run("look it up", &mut Vec::<LoopEvent>::new()).await;
///
/// // The cap stopped the turn before the second call; the call still has its result.
/// assert_eq!(end.stop_reason, StopReason::MaxTokens);
/// assert!(matches!(&agent.history()[2], Message::Tool(_)));
/// // Every message the loop appended reached the hook.
/// assert_eq!(*host.saved.lock().unwrap(), agent.history());
/// # }
/// # #[cfg(not(feature = "testing"))]
/// # fn main() {}
/// ```
#[async_trait]
pub trait AgentHooks: ToolServices {
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
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
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
}

/// Milliseconds since the Unix epoch, by the wall clock; 0 if the clock is
/// set before 1970. What [`WallClock`] reads.
pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

/// PLUG POINT: the clock the turn guard's time cap reads, in milliseconds
/// since the epoch ([`SharedDeps::clock`]). A host that pauses runs gives one
/// that stands still while paused. A closure `Fn() -> u64` is a clock.
pub trait Clock: Send + Sync {
    /// Now, in milliseconds since the epoch.
    fn now_ms(&self) -> u64;
}

impl<F: Fn() -> u64 + Send + Sync> Clock for F {
    fn now_ms(&self) -> u64 {
        self()
    }
}

/// The wall clock ([`now_ms`]): [`AgentDeps::new`]'s default.
#[derive(Debug, Default, Clone, Copy)]
pub struct WallClock;

impl Clock for WallClock {
    fn now_ms(&self) -> u64 {
        now_ms()
    }
}

/// Hooks that change nothing: [`AgentDeps::new`]'s default.
#[derive(Debug, Default)]
pub struct NoHooks;
impl ToolServices for NoHooks {}
impl AgentHooks for NoHooks {}

/// Everything the loop needs: what [`Agent::new`] takes. An agent's own
/// tools and system prompt, and what it runs on besides ([`SharedDeps`]),
/// which the agents of one conversation share.
///
/// Cheap to clone (every field is an `Arc` or small). The loop reads the
/// models, tools, prompt and hooks again before every model call, so a
/// source that changes behind its `Arc` is seen at the next call.
#[derive(Clone)]
pub struct AgentDeps {
    /// The tools: which to advertise, and which a call may run.
    pub tools: Arc<dyn ToolSource>,
    /// The system prompt, built again before every model call.
    pub system_prompt: Arc<dyn SystemPrompt>,
    /// The models, the workspace, the host's hooks and the rest.
    pub shared: SharedDeps,
}

/// What an agent runs on besides its tools and system prompt. Main and its
/// subagents share one (a subagent hub gets it from its parent), so the
/// whole tree runs on the same models, files, hooks and ledger.
#[derive(Clone)]
pub struct SharedDeps {
    /// Resolves [`Agent::model_spec`] to a provider, before every model call.
    pub models: Arc<dyn ModelResolver>,
    /// The working directory: tools resolve relative paths against it.
    pub cwd: PathBuf,
    /// Every file and shell operation a tool performs goes through this.
    pub workspace: Arc<dyn Workspace>,
    /// Where tool results too big for the context are written (through the
    /// workspace); None = the model gets a preview and is told the rest was not saved.
    pub spill_dir: Option<PathBuf>,
    /// The host's say in the loop; [`NoHooks`] unless set.
    pub hooks: Arc<dyn AgentHooks>,
    /// What the host knows about each model: inputs, reasoning;
    /// [`NoFacts`] unless set.
    pub facts: Arc<dyn ModelFacts>,
    /// The turn caps for each model; none unless set.
    pub turn_caps: Arc<dyn TurnCapsSource>,
    /// The clock the turn's time cap reads; [`WallClock`] unless set.
    pub clock: Arc<dyn Clock>,
    /// What the conversation has spent across turns; every model call and tool spend is charged here.
    /// None = no conversation budget, only the turn caps.
    pub ledger: Option<Arc<Mutex<Ledger>>>,
    /// Provider-side web search for every model call; None = off.
    pub web_search: Option<WebSearchOptions>,
    /// The host's way past a turn cap or the request cap, added to the
    /// turn's error when one stops it; none unless set.
    pub remedies: Remedies,
    /// Where durable runs keep their records ([`Agent::run_durable`]).
    /// None = no run is durable: [`Agent::run`] never touches a store either way.
    pub run_store: Option<Arc<dyn RunStore>>,
}

impl SharedDeps {
    /// The minimum: models, a working directory and a workspace. Everything
    /// else off: no spill directory, [`NoHooks`], [`NoFacts`], no turn caps,
    /// the [`WallClock`], no ledger, no web search, no remedies, no run store.
    pub fn new(
        models: Arc<dyn ModelResolver>,
        cwd: impl Into<PathBuf>,
        workspace: Arc<dyn Workspace>,
    ) -> Self {
        Self {
            models,
            cwd: cwd.into(),
            workspace,
            spill_dir: None,
            hooks: Arc::new(NoHooks),
            facts: Arc::new(NoFacts),
            turn_caps: Arc::new(TurnCaps::default()),
            clock: Arc::new(WallClock),
            ledger: None,
            web_search: None,
            remedies: Remedies::default(),
            run_store: None,
        }
    }
}

impl AgentDeps {
    /// The minimum: models, tools, a system prompt and a workspace; the rest
    /// as [`SharedDeps::new`] leaves it.
    pub fn new(
        models: Arc<dyn ModelResolver>,
        tools: Arc<dyn ToolSource>,
        system_prompt: Arc<dyn SystemPrompt>,
        cwd: impl Into<PathBuf>,
        workspace: Arc<dyn Workspace>,
    ) -> Self {
        Self {
            tools,
            system_prompt,
            shared: SharedDeps::new(models, cwd, workspace),
        }
    }
}
