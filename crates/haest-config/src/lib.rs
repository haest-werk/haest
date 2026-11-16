//! haest's settings loader, for a host that wants haest's files, variables and
//! flags. The runtime never reads any of them: it is handed what
//! [`Loader::load`] returns (or what a host writes out itself,
//! `haest_settings::Settings`).
//!
//! One name per setting, three spellings: `skill_catalog` in a TOML file,
//! `HAEST_SKILL_CATALOG` in the environment, `--skill-catalog` on the command
//! line. A host picks the layers and their order ([`Loader`]); the `haest`
//! command reads [`Loader::standard`]: the defaults, the XDG dirs, your
//! `<config dir>/config.toml`, a trusted project's `.haest/config.toml`, the
//! profile, `HAEST_*` variables, flags. `profile = "claude"` turns on
//! everything Claude Code has that haest reads.
//!
//! Beside the settings, [`Loader::load`] reads what the runtime is handed with
//! them: the provider keys (the environment), `providers.json`, OpenRouter's
//! routing (`provider_routing`, parsed), a trusted project's `.mcp.json`, and
//! the environment a child process gets (`env_passthrough`, and `env` on top).
//!
//! [`DEFS`] is the one list: a host builds its flags from it.

mod fields;
mod file;
mod handed;
mod loader;
mod profiles;
mod registry;
mod source;
mod stack;
#[cfg(test)]
mod tests;
mod value;

use std::collections::HashMap;

use haest_core::ChildEnv;
use haest_mcp::McpServerConfig;
use haest_providers::{CustomProviders, RoutingEnv};

pub use fields::{CORE_VARS, Config, EnvPassthrough, get, set};
pub use loader::Loader;
pub use registry::{DEFS, Def, Kind, Scope};
pub use source::{ProjectFile, Resolved, Source};
pub use value::env_var;

/// The settings, where each came from, and what the runtime is handed with them.
#[derive(Debug, Clone)]
pub struct Loaded {
    pub config: Config,
    pub resolved: Resolved,
    /// What a process haest starts gets of the environment (`env_passthrough`).
    pub child_env: ChildEnv,
    /// The variables passed on purpose (`env`), filled in: every process haest
    /// starts gets them over `child_env`, the sandbox's shell included.
    pub env: ChildEnv,
    /// The environment, as the source of the provider keys.
    pub keys: HashMap<String, String>,
    /// `provider_routing`, parsed.
    pub routing: Option<RoutingEnv>,
    /// `<config dir>/providers.json`.
    pub providers: CustomProviders,
    /// A trusted project's `.mcp.json`, with `mcp` on.
    pub mcp_servers: Vec<McpServerConfig>,
    /// What the host should tell the human: a setting asked for and not honoured.
    pub notes: Vec<String>,
}
