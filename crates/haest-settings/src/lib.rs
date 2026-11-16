//! haest's settings as plain typed values: everything a conversation is built
//! from, with no files, no environment and no flags.
//!
//! [`Settings`] is one struct of public fields. [`Settings::bare`] fills it the
//! way haest starts: every capability off until asked for (no tools, no
//! instruction files, no skills, no Code, no agents, no web search, no mods)
//! and every brake on (approvals, the caps in [`Limits`], the sandbox
//! workspace). A host changes the fields it wants and keeps the rest with
//! struct update syntax (`..Settings::bare(cwd, home)`).
//!
//! # Where it fits
//!
//! Through the facade this crate is `haest::settings` (the `settings` feature,
//! on by default); the types are the same. Depend on `haest-settings` directly
//! when you need only the values: it pulls in `haest-core` and nothing else, no
//! tools and no HTTP.
//!
//! A [`Settings`] gets its values one of two ways:
//!
//! - **Written out in Rust**, as in the examples below. Nothing is read, so the
//!   process's environment cannot change what a run may do. A host with a few
//!   fixed kinds of run writes one `Settings` per kind.
//! - **Loaded** by [haest-config](https://docs.rs/haest-config/): its `Loader`
//!   starts from the defaults you hand it (`Settings::bare`, or your own) and
//!   layers haest's TOML files, `HAEST_*` variables and flags on top. A setting
//!   has the same name in all three as the field here (`skill_catalog`,
//!   `HAEST_SKILL_CATALOG`, `--skill-catalog`).
//!
//! [haest-runtime](https://docs.rs/haest-runtime/) builds a whole conversation
//! from a `Settings`. A host that builds its own takes the parts it needs:
//! [`Settings::turn_caps_for`] and [`Settings::conversation_caps`] for the loop's caps,
//! [`WordSet::has`] on [`Settings::tools`] for haest-tools'
//! `builtin_tools_where`.
//!
//! The settings that decide what a run is handed rather than how it behaves
//! (`profile`, `provider_routing`, `mcp`, `env`, `env_passthrough`,
//! `trusted_dirs`) are not here: they belong to the loader and the runtime's
//! inputs.
//!
//! # Examples
//!
//! A run that may read and search the project, and nothing else:
//!
//! ```
//! use haest_settings::{BuiltinTools, Settings};
//!
//! let settings = Settings {
//!     tools: BuiltinTools { read: true, grep: true, glob: true, ..BuiltinTools::NONE },
//!     ..Settings::bare("/work/project", "/home/me")
//! };
//! assert!(settings.tools.read && !settings.tools.write && !settings.tools.bash);
//! assert!(settings.approvals, "bare keeps every brake on");
//! ```
//!
//! A tool list as a config file or a flag spells it. The words come back in
//! one fixed order, and a typo is refused:
//!
//! ```
//! use haest_settings::{BuiltinTools, WordSet};
//!
//! let tools = BuiltinTools::from_words(["bash", "read"]).unwrap();
//! assert_eq!(tools.words(), ["read", "bash"]);
//! assert_eq!(BuiltinTools::from_words(["bsh"]).unwrap_err().word, "bsh");
//! ```
//!
//! The caps for a model with a one-million-token window, when the token caps
//! are left unset:
//!
//! ```
//! use haest_settings::Settings;
//!
//! let settings = Settings::bare("/w", "/h");
//! let window = settings.window(Some(1_000_000));
//! let caps = settings.turn_caps_for(window);
//! assert_eq!(caps.max_request_tokens, 900_000); // 90% of the window
//! assert_eq!(caps.max_turn_tokens, 10_000_000); // 10x the window
//! ```

#![cfg_attr(docsrs, feature(doc_cfg))]
#![warn(missing_docs)]

mod sets;
mod settings;
#[cfg(test)]
mod tests;

pub use haest_core::{context::ContextSources, skills::SkillSources, words::WordSet};
pub use sets::BuiltinTools;
pub use settings::{DEFAULT_MODEL, Limits, Settings};
