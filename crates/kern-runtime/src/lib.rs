//! kern assembled from what a host hands it ([`RuntimeInputs`]): the
//! [`Settings`](kern_settings::Settings), the [`World`] (the real disk with the
//! environment a child gets, or the host's own Fs), the provider keys, the
//! custom providers, the MCP servers, the routing. The runtime reads no file,
//! variable or flag of its own; kern-config is the opt-in loader that fills
//! these in from kern's files, `KERN_*` variables and flags.
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
//! never built. Trust is the loader's: it lists a project's MCP servers and
//! turns its mods on only in a trusted dir.

mod agents;
mod build;
mod host;
pub mod inbox;
pub mod mods;
pub mod prompt;
pub mod sessions;
mod store;
#[cfg(test)]
mod testkit;
#[cfg(test)]
mod tests;
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
pub use build::{BuildOptions, Runtime, RuntimeInputs, build};
pub use host::{Host, session_usd};
pub use store::{init_store, open_store};
pub use ui::Ui;
pub use world::World;
