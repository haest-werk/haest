//! What a host hands [`build`](crate::build) for one conversation: the
//! inputs ([`RuntimeInputs`]) and what it decides per run ([`BuildOptions`],
//! [`Resume`]).

use std::sync::Arc;

use haest_settings::Settings;

use crate::{
    haest::{
        ChildEnv, KeySource,
        http::{HttpClient, ReqwestClient},
        mcp::McpServerConfig,
        providers::{CustomProviders, RoutingScopes},
    },
    world::World,
};

/// Everything a conversation is built from. haest reads no file, variable or
/// flag of its own: what is not in here does not exist for it.
///
/// Start from [`RuntimeInputs::new`] (the settings and the world, nothing
/// else) and set the fields the host has. A host that loads haest's own
/// files with haest-config gets all of them from its `Loaded` with the
/// facade's `haest::runtime_inputs`.
pub struct RuntimeInputs {
    /// Every setting of the run: which capabilities are on, the limits, the
    /// model, where the sessions go.
    pub settings: Settings,
    /// Where the agent's files are, and whether it may start real processes.
    pub world: World,
    /// Variables the host passes on purpose (haest-config's `env` setting):
    /// every process haest starts gets them over the world's `child_env`, the
    /// sandbox's shell included (it gets nothing else of the host's
    /// environment).
    pub passed_env: ChildEnv,
    /// What the host answers for a provider variable (`OPENROUTER_API_KEY`,
    /// `ANTHROPIC_BASE_URL`, ...): its environment, a vault, a fixed map. A
    /// closure `Fn(&str) -> Option<String>` and a `HashMap<String, String>`
    /// are both a [`KeySource`]. GenerateImage asks it for
    /// `OPENROUTER_API_KEY` too.
    pub keys: Arc<dyn KeySource>,
    /// providers.json's entries, by name: providers beside the built-in ones.
    pub providers: CustomProviders,
    /// The MCP servers to start. In a [`World::files`] the stdio ones are skipped
    /// with a warning: they are real processes.
    pub mcp_servers: Vec<McpServerConfig>,
    /// OpenRouter's `provider` routing; None = none goes out.
    pub routing: Option<RoutingScopes>,
    /// How the model calls and GenerateImage's requests are sent: the host's
    /// own client (a gateway, a recorder) or the shared [`ReqwestClient`].
    /// WebFetch keeps its own client, which checks every address it connects
    /// to; MCP servers keep rmcp's.
    pub http: Arc<dyn HttpClient>,
}

impl RuntimeInputs {
    /// The settings in this world, and nothing else: no variables passed, no
    /// keys, no custom providers, no MCP servers, no routing, the shared
    /// [`ReqwestClient`] (built on the first request).
    ///
    /// With no keys, [`build`](crate::build) fails unless the model's provider needs none:
    /// set [`keys`](Self::keys) for the providers the settings use.
    pub fn new(settings: Settings, world: World) -> Self {
        let no_keys = |_: &str| None;
        Self {
            settings,
            world,
            passed_env: ChildEnv::default(),
            keys: Arc::new(no_keys),
            providers: CustomProviders::new(),
            mcp_servers: Vec::new(),
            routing: None,
            http: Arc::new(ReqwestClient::new()),
        }
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
/// and whether the settings' model and effort were asked for on purpose.
///
/// The default is a new session on the settings' model and effort.
#[derive(Debug, Clone, Default)]
pub struct BuildOptions {
    /// The session to write: a new one, or one to go on with.
    pub resume: Resume,
    /// The settings' model was asked for on purpose for this run (a flag):
    /// a resume goes on with it, not with the one the session saved.
    pub model_spec_given: bool,
    /// The same, for the effort.
    pub effort_given: bool,
}
