//! What a host hands [`build`](crate::build) for one conversation: the
//! inputs ([`RuntimeInputs`]) and what it decides per run ([`BuildOptions`],
//! [`Resume`]).

use std::fmt;

use haest_settings::Settings;

use crate::{
    haest::{
        ChildEnv, KeySourceRef,
        http::HttpClientRef,
        mcp::McpServerConfig,
        providers::{CustomProviders, RoutingScopes},
    },
    world::World,
};

/// Everything a conversation is built from. haest reads no file, variable or
/// flag of its own: what is not in here does not exist for it.
///
/// Start from [`RuntimeInputs::new`] (the settings and the world, nothing
/// else) and set what the host has with the `with_*` setters; each part is
/// read back with the getter of its name. A host that loads haest's own
/// files with haest-config gets all of them from its `Loaded` with the
/// facade's `haest::runtime_inputs`.
///
/// ```
/// use std::collections::HashMap;
///
/// use haest_core::fs::MemFs;
/// use haest_runtime::{RuntimeInputs, World};
/// use haest_settings::Settings;
///
/// // A fixed map of keys; a closure or a vault is a `KeySource` too.
/// let keys = HashMap::from([("OPENROUTER_API_KEY".to_string(), "sk-or-example".to_string())]);
/// let inputs = RuntimeInputs::new(Settings::bare("/work", "/home/me"), World::files(MemFs::new()))
///     .with_keys(keys);
/// assert_eq!(inputs.keys().key("OPENROUTER_API_KEY").as_deref(), Some("sk-or-example"));
/// assert!(inputs.mcp_servers().is_empty());
/// ```
pub struct RuntimeInputs {
    pub(crate) settings: Settings,
    pub(crate) world: World,
    pub(crate) passed_env: ChildEnv,
    pub(crate) keys: KeySourceRef,
    pub(crate) providers: CustomProviders,
    pub(crate) mcp_servers: Vec<McpServerConfig>,
    pub(crate) routing: Option<RoutingScopes>,
    pub(crate) http: HttpClientRef,
}

impl RuntimeInputs {
    /// The settings in this world, and nothing else: no variables passed, no
    /// keys, no custom providers, no MCP servers, no routing, the default
    /// client ([`HttpClientRef::default`]: the shared `ReqwestClient`, built
    /// on the first request).
    ///
    /// With no keys, [`build`](crate::build) fails unless the model's provider needs none:
    /// hand in the [`keys`](Self::with_keys) the settings' providers use.
    pub fn new(settings: Settings, world: World) -> Self {
        let no_keys = |_: &str| None;
        Self {
            settings,
            world,
            passed_env: ChildEnv::default(),
            keys: KeySourceRef::new(no_keys),
            providers: CustomProviders::new(),
            mcp_servers: Vec::new(),
            routing: None,
            http: HttpClientRef::default(),
        }
    }

    /// Every setting of the run: which capabilities are on, the limits, the
    /// model, where the sessions go.
    #[must_use]
    pub fn with_settings(mut self, settings: Settings) -> Self {
        self.settings = settings;
        self
    }

    /// Where the agent's files are, and whether it may start real processes.
    #[must_use]
    pub fn with_world(mut self, world: World) -> Self {
        self.world = world;
        self
    }

    /// Variables the host passes on purpose (haest-config's `env` setting):
    /// every process haest starts gets them over the world's `child_env`, the
    /// sandbox's shell included (it gets nothing else of the host's
    /// environment).
    #[must_use]
    pub fn with_passed_env(mut self, passed_env: ChildEnv) -> Self {
        self.passed_env = passed_env;
        self
    }

    /// What the host answers for a provider variable (`OPENROUTER_API_KEY`,
    /// `ANTHROPIC_BASE_URL`, ...): its environment, a vault, a fixed map. A
    /// closure `Fn(&str) -> Option<String>` and a `HashMap<String, String>`
    /// are both a [`KeySource`](haest_core::KeySource); an `Arc` of one the
    /// host shares goes in as well. GenerateImage asks it for
    /// `OPENROUTER_API_KEY` too.
    #[must_use]
    pub fn with_keys(mut self, keys: impl Into<KeySourceRef>) -> Self {
        self.keys = keys.into();
        self
    }

    /// providers.json's entries, by name: providers beside the built-in ones.
    #[must_use]
    pub fn with_providers(mut self, providers: CustomProviders) -> Self {
        self.providers = providers;
        self
    }

    /// The MCP servers to start. In a [`World::files`] the stdio ones are
    /// skipped with a warning: they are real processes.
    #[must_use]
    pub fn with_mcp_servers(mut self, servers: impl IntoIterator<Item = McpServerConfig>) -> Self {
        self.mcp_servers = servers.into_iter().collect();
        self
    }

    /// OpenRouter's `provider` routing; None = none goes out.
    #[must_use]
    pub fn with_routing(mut self, routing: impl Into<Option<RoutingScopes>>) -> Self {
        self.routing = routing.into();
        self
    }

    /// How the model calls and GenerateImage's requests are sent: the host's
    /// own client (a gateway, a recorder; or an `Arc` of one it shares)
    /// instead of the default client. WebFetch keeps its own
    /// client, which checks every address it connects to; MCP servers keep
    /// rmcp's.
    #[must_use]
    pub fn with_http(mut self, http: impl Into<HttpClientRef>) -> Self {
        self.http = http.into();
        self
    }

    /// The settings ([`with_settings`](Self::with_settings)).
    pub fn settings(&self) -> &Settings {
        &self.settings
    }

    /// The world ([`with_world`](Self::with_world)).
    pub fn world(&self) -> &World {
        &self.world
    }

    /// The variables passed on purpose ([`with_passed_env`](Self::with_passed_env)).
    pub fn passed_env(&self) -> &ChildEnv {
        &self.passed_env
    }

    /// The provider keys ([`with_keys`](Self::with_keys)).
    pub fn keys(&self) -> &KeySourceRef {
        &self.keys
    }

    /// The custom providers ([`with_providers`](Self::with_providers)).
    pub fn providers(&self) -> &CustomProviders {
        &self.providers
    }

    /// The MCP servers to start ([`with_mcp_servers`](Self::with_mcp_servers)).
    pub fn mcp_servers(&self) -> &[McpServerConfig] {
        &self.mcp_servers
    }

    /// OpenRouter's routing ([`with_routing`](Self::with_routing)).
    pub fn routing(&self) -> Option<&RoutingScopes> {
        self.routing.as_ref()
    }

    /// The HTTP client the model calls go through ([`with_http`](Self::with_http)).
    pub fn http(&self) -> &HttpClientRef {
        &self.http
    }
}

/// The custom providers by name only (their own Debug): an entry may hold a key.
impl fmt::Debug for RuntimeInputs {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RuntimeInputs")
            .field("settings", &self.settings)
            .field("world", &self.world)
            .field("passed_env", &self.passed_env)
            .field("keys", &self.keys)
            .field("providers", &self.providers)
            .field("mcp_servers", &self.mcp_servers)
            .field("routing", &self.routing)
            .field("http", &self.http)
            .finish()
    }
}

/// Which session a run writes: a new one, or a saved one it goes on with.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum Resume {
    /// A new session.
    #[default]
    New,
    /// The session written last of those that started in the settings' `cwd`.
    Latest,
    /// The saved session with this id ([`SessionInfo::id`](crate::SessionInfo::id)).
    Id(String),
}

/// What a host decides per run, beside the settings: which session to write,
/// and which model and effort a resumed one goes on with.
///
/// The default is a new session on the settings' model and effort; a resume
/// goes on with the session's own.
///
/// ```
/// use haest_runtime::{BuildOptions, OnResume, Prefer, Resume};
///
/// // `-r -m a:b`: the latest session, on the model the flag picked.
/// let opts = BuildOptions {
///     resume: Resume::Latest,
///     on_resume: OnResume { model: Prefer::Settings, effort: Prefer::Saved },
/// };
/// assert_ne!(opts.on_resume, OnResume::default());
/// ```
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BuildOptions {
    /// The session to write: a new one, or one to go on with.
    pub resume: Resume,
    /// Which model and effort a resume goes on with. A new session starts
    /// on the settings' either way.
    pub on_resume: OnResume,
}

/// Which model and which effort a resumed session goes on with.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct OnResume {
    /// The model.
    pub model: Prefer,
    /// The effort.
    pub effort: Prefer,
}

/// Which of two a resume goes on with: the one the session saved, or the
/// settings' one.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Prefer {
    /// The one the session saved (the settings' when it saved none).
    #[default]
    Saved,
    /// The settings' one, asked for on purpose for this run (a flag). It is
    /// recorded in the session, so the next resume goes on with it too.
    Settings,
}
