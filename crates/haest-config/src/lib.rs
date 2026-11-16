//! haest's settings loader: it reads haest's TOML config files, its `HAEST_*`
//! variables and its flags, laid one over the other in the order a host adds
//! them, into the values haest-runtime is handed.
//!
//! The runtime never reads a file, a variable or a flag of its own. A host
//! either writes its settings out (`haest_settings::Settings { .. }`, no loader
//! at all) or takes this loader and gets what the `haest` command reads:
//! [`Loader::load`] returns the settings and everything the runtime is handed
//! beside them ([`Loaded`]). `haest::config` is this crate, behind the
//! facade's `config` feature; depend on haest-config directly to take the
//! loader without the facade.
//!
//! # One name, three spellings
//!
//! Every setting has one name, written three ways: `skill_catalog` in a TOML
//! file, `HAEST_SKILL_CATALOG` in the environment, `--skill-catalog` on the
//! command line. [`DEFS`] lists them all, once: a host builds its flags and
//! its `--help` from it. A value is checked against its setting the same way
//! whichever layer it came from, and a wrong one is an error naming where it
//! was (`HAEST_MAX_TURN_USD "lots" must be a number >= 0`), never read as a
//! default. Every setting, with its three spellings and its default, is in
//! [docs/settings.md](https://github.com/haest-werk/haest/blob/main/docs/settings.md).
//!
//! # Layers
//!
//! [`Loader::standard`] lays these, each winning over the ones before it:
//!
//! 1. the defaults the host starts from ([`Loader::new`]; `Settings::bare` is
//!    bare haest: every capability off, every brake on);
//! 2. `$XDG_CONFIG_HOME/haest` and `$XDG_DATA_HOME/haest` as the config and
//!    data dirs, when those are set ([`Loader::xdg_dirs`]);
//! 3. your `<config dir>/config.toml` ([`Loader::user_file`]);
//! 4. the nearest `.haest/config.toml` from the working dir up, read only in a
//!    trusted dir ([`Loader::project_file`]);
//! 5. the profile `profile` names ([`Loader::profile`]);
//! 6. `HAEST_*` variables ([`Loader::env`]);
//! 7. flags ([`Loader::flags`]).
//!
//! A host picks its own layers and their order with [`Loader::new`] and the
//! same methods; a layer it does not add is not read. Nothing is read until
//! [`Loader::load`].
//!
//! # Profiles
//!
//! A profile is a named set of settings, picked with `profile` in any layer.
//! haest ships one: `claude`, which turns on everything Claude Code has that
//! haest reads (instruction files, skills, every tool, subagents, web search,
//! the project's MCP servers, the real workspace, the whole environment for
//! what haest starts). A config file's `[profiles.<name>]` table adds to a
//! built-in profile (its keys win) or defines a new one. The profile's values
//! win over the config files' own and lose to variables and flags.
//!
//! # Trust
//!
//! A project's files are the repo's, not yours: its `.haest/config.toml`, its
//! `.mcp.json` (which starts programs) and its `.haest/mods` (which run code)
//! are read only when the working dir is inside one of `trusted_dirs`. Only
//! your own file, a variable or a flag can set `trusted_dirs`: a project's
//! file can't trust itself ([`Scope::User`]). A project file skipped for want
//! of trust is a note in [`Loaded::notes`], and [`Resolved::project_file`]
//! says which file it was.
//!
//! # The environment
//!
//! [`Loaded`] also carries what a process haest starts (a shell, a background
//! job, an MCP server) gets of haest's environment: `env_passthrough` picks
//! `none`, the [`core`](CORE_VARS) variables or `all`, and `env` passes named
//! variables on purpose, `${NAME}` filled in from haest's own. The provider
//! keys come from the same variables, and stay with haest.
//! [docs/environment.md](https://github.com/haest-werk/haest/blob/main/docs/environment.md)
//! has the whole story.
//!
//! # Examples
//!
//! What the `haest` command loads, over files in memory: a user config, a
//! trusted project's config, a variable and a flag.
//!
//! ```
//! use std::collections::HashMap;
//!
//! use haest_config::{Loader, ProjectFile, Source};
//! use haest_core::fs::MemFs;
//! use haest_settings::{BuiltinTools, Settings};
//!
//! # #[tokio::main(flavor = "current_thread")]
//! # async fn main() -> Result<(), haest_config::ConfigError> {
//! let fs = MemFs::with_files([
//!     (
//!         "/home/me/.config/haest/config.toml",
//!         "tools = [\"read\", \"grep\"]\nmax_turn_usd = 1\ntrusted_dirs = [\"~/projects\"]",
//!     ),
//!     ("/home/me/projects/app/.haest/config.toml", "tools = \"all\""),
//! ]);
//! let env = HashMap::from([
//!     ("HOME".to_string(), "/home/me".to_string()),
//!     ("HAEST_MAX_TURN_USD".to_string(), "3".to_string()),
//!     ("OPENROUTER_API_KEY".to_string(), "sk-or-example".to_string()),
//! ]);
//! let flags = [("model".to_string(), "anthropic:claude-opus-5-5".to_string())];
//!
//! let defaults = Settings::bare("/home/me/projects/app", "/home/me");
//! let loaded = Loader::standard(defaults, &env, &flags).load(&fs).await?;
//! let settings = &loaded.config.settings;
//!
//! // ~/projects is trusted, so the project's file was read, over the user's.
//! assert!(loaded.resolved.trusted);
//! assert!(matches!(loaded.resolved.project_file, ProjectFile::Read(_)));
//! assert_eq!(settings.tools, BuiltinTools::ALL);
//!
//! // The variable beats the file, the flag beats everything, and each value
//! // says where it came from.
//! assert_eq!(settings.caps.max_turn_usd, 3.0);
//! assert_eq!(
//!     loaded.resolved.source("max_turn_usd"),
//!     Source::Env("HAEST_MAX_TURN_USD".into())
//! );
//! assert_eq!(settings.model_spec, "anthropic:claude-opus-5-5");
//! assert_eq!(loaded.resolved.source("model").to_string(), "--model");
//!
//! // The provider key is the runtime's to use; a process haest starts gets
//! // only the core variables (`env_passthrough = core`, the default).
//! assert_eq!(loaded.keys["OPENROUTER_API_KEY"], "sk-or-example");
//! assert_eq!(loaded.child_env.get("HOME"), Some("/home/me"));
//! assert_eq!(loaded.child_env.get("OPENROUTER_API_KEY"), None);
//! assert!(loaded.notes.is_empty());
//! # Ok(())
//! # }
//! ```

#![cfg_attr(docsrs, feature(doc_cfg))]
#![warn(missing_docs)]

mod error;
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
use haest_providers::{CustomProviders, RoutingScopes};

pub use error::{ConfigError, SetError};
pub use fields::{BrokerAccess, CORE_VARS, Config, ControlSocket, EnvPassthrough};
pub use loader::Loader;
pub use registry::{DEFS, Def, Kind, Scope};
pub use source::{ProjectFile, Resolved, Source};
pub use value::env_var;

/// What [`Loader::load`] returns: the settings, where each came from, and
/// what the runtime is handed with them.
///
/// Each field goes into haest-runtime's `RuntimeInputs` as it is; this is
/// what the `haest` command does:
///
/// | `RuntimeInputs` | from `Loaded` |
/// |---|---|
/// | `settings` | `config.settings` |
/// | `world` | `World::Disk { child_env }` |
/// | `passed_env` | `passed_env` |
/// | `keys` | `Arc::new(keys)` (a `HashMap` is a `KeySource`) |
/// | `providers` | `providers` |
/// | `mcp_servers` | `mcp_servers` |
/// | `routing` | `routing` |
/// | `http` | not loaded: the host's own client, or `Arc::new(ReqwestClient::new())` |
///
/// Show the human every [`notes`](Self::notes) line before the run starts.
#[derive(Debug, Clone)]
pub struct Loaded {
    /// Every setting's value: the runtime's settings, and the ones only the
    /// loader reads.
    pub config: Config,
    /// Where each value came from, and what trust decided.
    pub resolved: Resolved,
    /// What a process haest starts gets of the environment (`env_passthrough`).
    pub child_env: ChildEnv,
    /// The variables passed on purpose (`env`), filled in: every process haest
    /// starts gets them over `child_env`, the sandbox's shell included.
    pub passed_env: ChildEnv,
    /// The environment [`Loader::env`] was given, as the source of the
    /// provider keys; empty when the loader reads no variables.
    pub keys: HashMap<String, String>,
    /// `provider_routing`, parsed. None: no routing goes out (it is unset, or
    /// malformed and a note says so).
    pub routing: Option<RoutingScopes>,
    /// `<config dir>/providers.json`'s entries; empty when there is no file.
    pub providers: CustomProviders,
    /// A trusted project's `.mcp.json` servers, with `mcp` on; empty otherwise.
    pub mcp_servers: Vec<McpServerConfig>,
    /// What the host should tell the human: a setting asked for and not
    /// honoured (an untrusted project's file, mods or MCP servers skipped, a
    /// variable `env` names that has no value, a malformed `provider_routing`,
    /// a profile named but not read).
    pub notes: Vec<String>,
}
