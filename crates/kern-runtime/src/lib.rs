//! kern assembled from its [`Settings`](kern_settings::Settings): what turns a
//! settings file into a running conversation. Every host builds on it -- the
//! CLI with a terminal [`Ui`], brain with its own pieces.
//!
//! Two ways in:
//!   - [`build`]: the whole conversation (providers, workspace, tools, MCP,
//!     mods, subagents, inbox, session file) behind one [`Ui`] and the host's
//!     approval [`Prompter`](kern_mods::Prompter).
//!   - the pieces, one by one: [`tools::own_tools`],
//!     [`prompt::instruction_opts`], [`prompt::skill_opts`],
//!     [`world::workspace_for`], [`world::connect_mcp`], [`mods::load`] -- for
//!     a host that owns some parts itself (its own models, Fs or tools). A host
//!     that wants only the built-ins picks them with
//!     `kern_tools::builtin_tools_where(|w| settings.tools.has(w), ..)` and
//!     needs no runtime at all.
//!
//! Every switch is honoured here, once: a capability the settings leave off is
//! never built, and what lives in the project (its `.mcp.json`, its mods) runs
//! only in a trusted dir.

mod agents;
mod build;
mod host;
pub mod inbox;
pub mod mods;
pub mod prompt;
pub mod sessions;
#[cfg(test)]
mod testkit;
pub mod tools;
mod ui;
pub mod world;

/// The names the runtime's code uses, as kern-lib spells them.
mod kern {
    pub use kern_code as code;
    pub use kern_core::*;
    pub use kern_mcp as mcp;
    pub use kern_mods as mods;
    pub use kern_providers as providers;
    pub use kern_tools as tools;
    pub use kern_workspace as workspace;
}

pub use agents::Agents;
pub use build::{BuildOptions, Runtime, build, init_store, open_store};
pub use host::{Host, now_ms, session_usd};
pub use ui::Ui;
