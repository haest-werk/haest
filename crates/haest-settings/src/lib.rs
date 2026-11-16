//! haest's settings as plain values: what a conversation is built from.
//!
//! No files, no environment, no flags here. A host writes the values out:
//!
//! ```
//! use haest_settings::{BuiltinTools, Settings};
//!
//! let s = Settings {
//!     tools: BuiltinTools { read: true, bash: true, ..BuiltinTools::NONE },
//!     agents: true,
//!     ..Settings::bare("/work", "/home/me")
//! };
//! assert!(s.tools.read && !s.tools.write && s.approvals);
//! ```
//!
//! or has a loader fill them in: haest-config reads TOML files, `HAEST_*`
//! variables and flags, by the same names.
//!
//! Bare means every capability off until asked for (no tools, no context
//! files, no skills, no mods) and every brake on (approvals, the spend and turn
//! caps, the sandbox).

mod sets;
mod settings;
#[cfg(test)]
mod tests;

pub use haest_core::{context::ContextSources, skills::SkillSources, words::WordSet};
pub use sets::BuiltinTools;
pub use settings::{DEFAULT_MODEL, Limits, Settings};
