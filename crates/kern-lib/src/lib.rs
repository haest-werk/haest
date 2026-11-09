//! kern-lib: kern as one dependency. `kern::core` is always there; the rest is
//! behind features so a host takes only what it uses:
//!
//! ```toml
//! # everything
//! kern-lib = { git = "...", package = "kern-lib" }
//! # a host with its own provider transport, files and tools (brain): the
//! # transport plugs into kern-providers, its Fs into the sandbox, and it keeps
//! # kern's Read / Write / Edit / Bash and agent tools
//! kern-lib = { git = "...", package = "kern-lib", default-features = false, features = ["providers", "workspace", "tools"] }
//! ```
//!
//! The example `crates/kern-lib/examples/brain` wires every plug point that way.

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
#[cfg(feature = "tools")]
pub use kern_tools as tools;
#[cfg(feature = "workspace")]
pub use kern_workspace as workspace;
