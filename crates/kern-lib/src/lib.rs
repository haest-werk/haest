//! kern-lib: kern as one dependency. `kern::core` is always there; the rest is
//! behind features so a host takes only what it uses:
//!
//! ```toml
//! # everything
//! kern-lib = { git = "...", package = "kern-lib" }
//! # a host with its own provider transport, files and tools (brain): the
//! # transport plugs into kern-providers, its Fs into the sandbox, it keeps
//! # kern's Read / Write / Edit / Bash and agent tools, and its own TOML picks them
//! kern-lib = { git = "...", package = "kern-lib", default-features = false, features = ["providers", "workspace", "tools", "settings"] }
//! # the whole conversation, built from settings (what the CLI is made of)
//! kern-lib = { git = "...", package = "kern-lib", features = ["runtime"] }
//! ```
//!
//! The example `crates/kern-lib/examples/brain` wires every plug point that way.
//!
//! `settings` (on by default) reads kern's settings: TOML files, `KERN_*`
//! variables and flags, or a host's own TOML text through `settings::settle`.
//! Bare by default: every capability off until asked for. `runtime` builds a
//! whole conversation from them behind a host's UI. A host that owns some
//! parts itself takes the runtime's pieces, or only `settings` and
//! `tools::builtin_tools_where`.

pub use kern_core as core;
pub use kern_core::*;

#[cfg(feature = "code")]
pub use kern_code as code;
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
