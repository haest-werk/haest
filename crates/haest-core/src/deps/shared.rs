//! [`SharedDeps`]: what every agent of one conversation runs on.

use std::path::{Path, PathBuf};

use super::{AgentHooksRef, ClockRef, NoHooks, WallClock};
use crate::{
    caps::{TurnCaps, TurnCapsSourceRef},
    catalog::{ModelFactsRef, NoFacts},
    ledger::Ledger,
    provider::{ModelResolverRef, WebSearchOptions},
    remedy::Remedies,
    run::RunStoreRef,
    threshold::{Thresholds, TurnSite},
    tool::{NoServices, ToolServicesRef},
    workspace::WorkspaceRef,
};

/// What an agent runs on besides its tools and system prompt. Main and its
/// subagents share one (a subagent hub gets it from its parent), so the
/// whole tree runs on the same models, files, hooks and ledger.
///
/// Built by [`SharedDeps::new`] with every optional slot off, then set slot
/// by slot with the `with_*` setters; each getter is named after its slot.
/// Cheap to clone: every slot is a handle or small. The loop reads the
/// models, hooks and the rest again before every model call, so a source
/// that changes behind its handle is seen at the next call.
#[derive(Clone)]
pub struct SharedDeps {
    models: ModelResolverRef,
    cwd: PathBuf,
    workspace: WorkspaceRef,
    spill_dir: Option<PathBuf>,
    hooks: AgentHooksRef,
    services: ToolServicesRef,
    facts: ModelFactsRef,
    turn_caps: TurnCapsSourceRef,
    clock: ClockRef,
    ledger: Option<Ledger>,
    web_search: Option<WebSearchOptions>,
    remedies: Remedies,
    run_store: Option<RunStoreRef>,
    thresholds: Thresholds<TurnSite>,
}

impl SharedDeps {
    /// The minimum: models, a working directory and a workspace. Everything
    /// else off: no spill directory, [`NoHooks`], [`NoServices`],
    /// [`NoFacts`], no turn caps, the [`WallClock`], no ledger, no web
    /// search, no remedies, no run store; the guard's nudges are the only
    /// thresholds.
    pub fn new(
        models: impl Into<ModelResolverRef>,
        cwd: impl Into<PathBuf>,
        workspace: impl Into<WorkspaceRef>,
    ) -> Self {
        Self {
            models: models.into(),
            cwd: cwd.into(),
            workspace: workspace.into(),
            spill_dir: None,
            hooks: NoHooks.into(),
            services: NoServices.into(),
            facts: NoFacts.into(),
            turn_caps: TurnCaps::default().into(),
            clock: WallClock.into(),
            ledger: None,
            web_search: None,
            remedies: Remedies::default(),
            run_store: None,
            thresholds: Thresholds::default(),
        }
    }

    /// Resolves [`Agent::model_spec`](crate::Agent::model_spec) to a
    /// provider, before every model call.
    pub fn models(&self) -> &ModelResolverRef {
        &self.models
    }

    /// The working directory: tools resolve relative paths against it.
    pub fn cwd(&self) -> &Path {
        &self.cwd
    }

    /// Every file and shell operation a tool performs goes through this.
    pub fn workspace(&self) -> &WorkspaceRef {
        &self.workspace
    }

    /// Where tool results too big for the context are written (through the
    /// workspace); None = the model gets a preview and is told the rest was
    /// not saved.
    pub fn spill_dir(&self) -> Option<&Path> {
        self.spill_dir.as_deref()
    }

    /// The host's say in the loop; [`NoHooks`] unless set.
    pub fn hooks(&self) -> &AgentHooksRef {
        &self.hooks
    }

    /// What the host offers every tool (publishing, inner calls, debug
    /// lines), handed to each call's [`ToolContext`](crate::ToolContext);
    /// [`NoServices`] unless set.
    pub fn services(&self) -> &ToolServicesRef {
        &self.services
    }

    /// What the host knows about each model: inputs, reasoning; [`NoFacts`]
    /// unless set.
    pub fn facts(&self) -> &ModelFactsRef {
        &self.facts
    }

    /// The turn caps for each model; none unless set.
    pub fn turn_caps(&self) -> &TurnCapsSourceRef {
        &self.turn_caps
    }

    /// The clock the turn's time cap reads; [`WallClock`] unless set.
    pub fn clock(&self) -> &ClockRef {
        &self.clock
    }

    /// What the conversation has spent across turns; every model call and
    /// tool spend is charged here. None = no conversation budget, only the
    /// turn caps.
    pub fn ledger(&self) -> Option<&Ledger> {
        self.ledger.as_ref()
    }

    /// Provider-side web search for every model call; None = off.
    pub fn web_search(&self) -> Option<&WebSearchOptions> {
        self.web_search.as_ref()
    }

    /// The host's way past a turn cap or the request cap, added to the
    /// turn's error when one stops it; none unless set.
    pub fn remedies(&self) -> &Remedies {
        &self.remedies
    }

    /// Where durable runs keep their records
    /// ([`Agent::run_durable`](crate::Agent::run_durable)). None = no run is
    /// durable: [`Agent::run`](crate::Agent::run) never touches a store
    /// either way.
    pub fn run_store(&self) -> Option<&RunStoreRef> {
        self.run_store.as_ref()
    }

    /// What a turn checks as it runs ([`threshold`](crate::threshold)): the
    /// guard's nudges unless set. Wind-down is a set of them
    /// ([`Thresholds::wind_down`]). Every agent keeps its own record of
    /// which have fired.
    pub fn thresholds(&self) -> &Thresholds<TurnSite> {
        &self.thresholds
    }

    /// These deps resolving models with `models`.
    pub fn with_models(mut self, models: impl Into<ModelResolverRef>) -> Self {
        self.models = models.into();
        self
    }

    /// These deps working in `cwd`.
    pub fn with_cwd(mut self, cwd: impl Into<PathBuf>) -> Self {
        self.cwd = cwd.into();
        self
    }

    /// These deps on `workspace`.
    pub fn with_workspace(mut self, workspace: impl Into<WorkspaceRef>) -> Self {
        self.workspace = workspace.into();
        self
    }

    /// These deps spilling big tool results into `dir`.
    pub fn with_spill_dir(mut self, dir: impl Into<PathBuf>) -> Self {
        self.spill_dir = Some(dir.into());
        self
    }

    /// These deps with `hooks` as the host's say in the loop.
    pub fn with_hooks(mut self, hooks: impl Into<AgentHooksRef>) -> Self {
        self.hooks = hooks.into();
        self
    }

    /// These deps with `services` offered to every tool.
    pub fn with_services(mut self, services: impl Into<ToolServicesRef>) -> Self {
        self.services = services.into();
        self
    }

    /// These deps with `facts` about each model.
    pub fn with_facts(mut self, facts: impl Into<ModelFactsRef>) -> Self {
        self.facts = facts.into();
        self
    }

    /// These deps capping each turn by `caps` (a [`TurnCaps`] caps every
    /// model alike).
    pub fn with_turn_caps(mut self, caps: impl Into<TurnCapsSourceRef>) -> Self {
        self.turn_caps = caps.into();
        self
    }

    /// These deps reading time from `clock`.
    pub fn with_clock(mut self, clock: impl Into<ClockRef>) -> Self {
        self.clock = clock.into();
        self
    }

    /// These deps charging `ledger` for every model call and tool spend:
    /// a clone of the host's, which it keeps to read and raise.
    pub fn with_ledger(mut self, ledger: Ledger) -> Self {
        self.ledger = Some(ledger);
        self
    }

    /// These deps asking for provider-side web search on every model call
    /// (None turns it off).
    pub fn with_web_search(mut self, web_search: impl Into<Option<WebSearchOptions>>) -> Self {
        self.web_search = web_search.into();
        self
    }

    /// These deps adding `remedies` to the error of a turn a cap stopped.
    pub fn with_remedies(mut self, remedies: Remedies) -> Self {
        self.remedies = remedies;
        self
    }

    /// These deps keeping durable runs in `store`.
    pub fn with_run_store(mut self, store: impl Into<RunStoreRef>) -> Self {
        self.run_store = Some(store.into());
        self
    }

    /// These deps checking `thresholds` as every turn runs.
    pub fn with_thresholds(mut self, thresholds: Thresholds<TurnSite>) -> Self {
        self.thresholds = thresholds;
        self
    }
}
