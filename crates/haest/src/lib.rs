//! haest as one dependency. `haest::core` is always there; the rest is
//! behind features so a host takes only what it uses:
//!
//! ```toml
//! # the default: providers, workspace, tools, MCP, code, mods, settings
//! haest = { git = "..." }
//! # a host with its own provider transport, files and tools (brain): the
//! # transport plugs into haest-providers, its Fs into the sandbox, it keeps
//! # haest's Read / Write / Edit / Bash and agent tools, and writes its settings out
//! haest = { git = "...", default-features = false, features = ["providers", "workspace", "tools", "settings"] }
//! # the whole conversation, with haest's own files, variables and flags (what the CLI is made of)
//! haest = { git = "...", features = ["runtime", "config"] }
//! ```
//!
//! The example `crates/haest/examples/brain` wires every plug point that way.
//!
//! Nothing in haest reads a file, a variable or a flag of its own accord.
//! `settings` (on by default) is the values, plain: a host writes them out
//! (`Settings { tools: BuiltinTools::ALL, ..Settings::bare(cwd, home) }`).
//! `config` (opt-in) is the loader: haest's TOML files, `HAEST_*` variables and
//! flags, and what the runtime is handed beside the settings. `runtime`
//! builds a whole conversation from what it is handed, behind a host's UI. A
//! host that owns some parts itself takes the runtime's pieces, or only
//! `settings` and `tools::builtin_tools_where`.

pub use haest_core as core;
pub use haest_core::*;

#[cfg(feature = "code")]
pub use haest_code as code;
#[cfg(feature = "config")]
pub use haest_config as config;
#[cfg(feature = "mcp")]
pub use haest_mcp as mcp;
#[cfg(feature = "mods")]
pub use haest_mods as mods;
#[cfg(feature = "providers")]
pub use haest_providers as providers;
#[cfg(feature = "runtime")]
pub use haest_runtime as runtime;
#[cfg(feature = "settings")]
pub use haest_settings as settings;
#[cfg(feature = "tools")]
pub use haest_tools as tools;
#[cfg(feature = "workspace")]
pub use haest_workspace as workspace;
