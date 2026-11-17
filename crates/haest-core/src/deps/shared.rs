//! [`SharedDeps`]: what every agent of one conversation runs on.

use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

use super::{AgentDeps, AgentHooks, AgentHooksRef, Clock, ClockRef, NoHooks, WallClock};
use crate::{
    caps::{TurnCaps, TurnCapsSource, TurnCapsSourceRef},
    catalog::{ModelFacts, ModelFactsRef, NoFacts},
    ledger::Ledger,
    provider::{ModelResolverRef, WebSearchOptions},
    remedy::Remedies,
    run::RunStoreRef,
    threshold::{Thresholds, TurnSite},
    tool::{NoServices, ToolServices, ToolServicesRef},
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
#[derive(Clone, Debug)]
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

    /// These deps with `host` in every slot a host fills: the
    /// [`hooks`](Self::with_hooks), the [`services`](Self::with_services),
    /// the [`facts`](Self::with_facts), the [`turn caps`](Self::with_turn_caps)
    /// and the [`clock`](Self::with_clock), one value behind them all. Each
    /// trait's methods have defaults, so a host implements only what it says
    /// and leaves the rest an empty `impl`. A setter called after this
    /// replaces its own slot only.
    ///
    /// ```
    /// use haest_core::{
    ///     AgentHooks, Clock, ModelFacts, SharedDeps, ShutWorkspace, ToolServices, TurnCaps,
    ///     TurnCapsSource, provider::ModelError, ResolvedModel,
    /// };
    ///
    /// /// The host: its own turn caps and a clock that stands still.
    /// struct Host;
    /// impl AgentHooks for Host {}
    /// impl ToolServices for Host {}
    /// impl ModelFacts for Host {}
    /// impl TurnCapsSource for Host {
    ///     fn turn_caps(&self, _spec: &str) -> Option<TurnCaps> {
    ///         Some(TurnCaps { max_turn_steps: 10, ..TurnCaps::default() })
    ///     }
    /// }
    /// impl Clock for Host {
    ///     fn now_ms(&self) -> u64 {
    ///         1_000
    ///     }
    /// }
    ///
    /// let models = |spec: &str| -> Result<ResolvedModel, ModelError> {
    ///     Err(ModelError::BadSpec { spec: spec.into() })
    /// };
    /// let shared = SharedDeps::new(models, "/project", ShutWorkspace).with_host(Host);
    /// assert_eq!(shared.clock().now_ms(), 1_000);
    /// assert_eq!(shared.turn_caps().turn_caps("m").map(|c| c.max_turn_steps), Some(10));
    /// ```
    #[must_use]
    pub fn with_host<H>(self, host: H) -> Self
    where
        H: AgentHooks + ToolServices + ModelFacts + TurnCapsSource + Clock + 'static,
    {
        let host = Arc::new(host);
        self.with_hooks(Arc::clone(&host))
            .with_services(Arc::clone(&host))
            .with_facts(Arc::clone(&host))
            .with_turn_caps(Arc::clone(&host))
            .with_clock(host)
    }
}

impl AgentDeps {
    /// [`SharedDeps::with_host`], on these deps' shared part.
    #[must_use]
    pub fn with_host<H>(mut self, host: H) -> Self
    where
        H: AgentHooks + ToolServices + ModelFacts + TurnCapsSource + Clock + 'static,
    {
        self.shared = self.shared.with_host(host);
        self
    }
}

/// Each slot's setter: on [`SharedDeps`], and the same on [`AgentDeps`] for
/// its shared part. One list, so the two cannot drift apart.
macro_rules! setters {
    ($(
        $(#[$meta:meta])*
        $name:ident($($arg:ident: $ty:ty)?) { $slot:ident = $value:expr }
    )*) => {
        impl SharedDeps {
            $(
                $(#[$meta])*
                #[must_use]
                pub fn $name(mut self, $($arg: $ty)?) -> Self {
                    self.$slot = $value;
                    self
                }
            )*
        }

        impl AgentDeps {
            $(
                #[doc = concat!("[`SharedDeps::", stringify!($name), "`], on these deps' shared part.")]
                #[must_use]
                pub fn $name(mut self, $($arg: $ty)?) -> Self {
                    self.shared = self.shared.$name($($arg)?);
                    self
                }
            )*
        }
    };
}

setters! {
    /// These deps resolving models with `models`.
    with_models(models: impl Into<ModelResolverRef>) { models = models.into() }

    /// These deps working in `cwd`.
    with_cwd(cwd: impl Into<PathBuf>) { cwd = cwd.into() }

    /// These deps on `workspace`.
    with_workspace(workspace: impl Into<WorkspaceRef>) { workspace = workspace.into() }

    /// These deps spilling big tool results into `dir`. Without it there is
    /// no spill directory.
    with_spill_dir(dir: impl Into<PathBuf>) { spill_dir = Some(dir.into()) }

    /// These deps with `hooks` as the host's say in the loop.
    with_hooks(hooks: impl Into<AgentHooksRef>) { hooks = hooks.into() }

    /// These deps with `services` offered to every tool.
    with_services(services: impl Into<ToolServicesRef>) { services = services.into() }

    /// These deps with `facts` about each model.
    with_facts(facts: impl Into<ModelFactsRef>) { facts = facts.into() }

    /// These deps capping each turn by `caps` (a [`TurnCaps`] caps every
    /// model alike).
    with_turn_caps(caps: impl Into<TurnCapsSourceRef>) { turn_caps = caps.into() }

    /// These deps reading time from `clock`.
    with_clock(clock: impl Into<ClockRef>) { clock = clock.into() }

    /// These deps charging `ledger` for every model call and tool spend:
    /// a clone of the host's, which it keeps to read and raise. Without it
    /// there is no conversation budget.
    with_ledger(ledger: Ledger) { ledger = Some(ledger) }

    /// These deps asking for provider-side web search on every model call.
    /// Without it none is asked for.
    with_web_search(web_search: WebSearchOptions) { web_search = Some(web_search) }

    /// These deps adding `remedies` to the error of a turn a cap stopped:
    /// [`Remedies`], or the closure that writes them.
    with_remedies(remedies: impl Into<Remedies>) { remedies = remedies.into() }

    /// These deps keeping durable runs in `store`.
    with_run_store(store: impl Into<RunStoreRef>) { run_store = Some(store.into()) }

    /// These deps with no run store: no run is durable. (A store is a
    /// handle, so its setter takes the store itself, not an `Option`.)
    without_run_store() { run_store = None }

    /// These deps checking `thresholds` as every turn runs.
    with_thresholds(thresholds: Thresholds<TurnSite>) { thresholds = thresholds }
}
