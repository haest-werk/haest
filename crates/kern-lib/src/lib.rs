//! kern-lib: kern as one dependency. `kern::core` is always there; the rest is
//! behind features so a host takes only what it uses:
//!
//! ```toml
//! # the default: providers, workspace, tools, MCP, code, mods, settings
//! kern-lib = { git = "...", package = "kern-lib" }
//! # a host with its own provider transport, files and tools (brain): the
//! # transport plugs into kern-providers, its Fs into the sandbox, it keeps
//! # kern's Read / Write / Edit / Bash and agent tools, and writes its settings out
//! kern-lib = { git = "...", package = "kern-lib", default-features = false, features = ["providers", "workspace", "tools", "settings"] }
//! # the whole conversation, with kern's own files, variables and flags (what the CLI is made of)
//! kern-lib = { git = "...", package = "kern-lib", features = ["runtime", "config"] }
//! ```
//!
//! The example `crates/kern-lib/examples/brain` wires every plug point that way.
//!
//! Nothing in kern reads a file, a variable or a flag of its own accord.
//! `settings` (on by default) is the values, plain: a host writes them out
//! (`Settings { tools: BuiltinTools::ALL, ..Settings::bare(cwd, home) }`).
//! `config` (opt-in) is the loader: kern's TOML files, `KERN_*` variables and
//! flags, and what the runtime is handed beside the settings. `runtime`
//! builds a whole conversation from what it is handed, behind a host's UI. A
//! host that owns some parts itself takes the runtime's pieces, or only
//! `settings` and `tools::builtin_tools_where`.

pub use kern_core as core;
pub use kern_core::*;

#[cfg(feature = "code")]
pub use kern_code as code;
#[cfg(feature = "config")]
pub use kern_config as config;
#[cfg(feature = "mcp")]
pub use kern_mcp as mcp;
#[cfg(feature = "mods")]
pub use kern_mods as mods;
#[cfg(feature = "providers")]
pub use kern_providers as providers;
#[cfg(feature = "runtime")]
pub use kern_runtime as runtime;
#[cfg(feature = "settings")]
pub use kern_settings as settings;
#[cfg(feature = "tools")]
pub use kern_tools as tools;
#[cfg(feature = "workspace")]
pub use kern_workspace as workspace;
