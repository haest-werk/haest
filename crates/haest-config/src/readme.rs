//! The README's settings snippets, compiled: each block below is one of
//! them, and `tests/readme.rs` holds the README to these blocks. The README
//! writes the facade's paths, so a hidden line maps them: `haest::settings`
//! is haest-settings, `haest::config` this crate.

/// A host writes its settings out:
///
/// ```
/// # mod haest { pub use haest_settings as settings; }
/// use haest::settings::{BuiltinTools, Settings};
///
/// let settings = Settings {
///     tools: BuiltinTools { read: true, bash: true, ..BuiltinTools::NONE },
///     agents: true,
///     ..Settings::bare("/work", "/home/me")
/// };
/// # assert!(settings.tools.has("bash") && !settings.tools.has("grep") && settings.agents);
/// ```
///
/// or loads them, picking the layers and their order:
///
/// ```
/// # mod haest { pub use haest_config as config; pub use haest_settings as settings; }
/// # use haest::settings::Settings;
/// # #[tokio::main(flavor = "current_thread")]
/// # async fn main() -> Result<(), haest_config::ConfigError> {
/// # let (cwd, home) = (std::path::PathBuf::from("/work"), std::path::PathBuf::from("/home/me"));
/// # let env = std::collections::HashMap::from([("HAEST_TOOLS".to_string(), "read".to_string())]);
/// # let flags = [("model", "a:flag")];
/// # let fs = haest_core::fs::MemFs::from_files([("/home/me/.config/haest/config.toml", "agents = true")]);
/// use haest::config::Loader;
///
/// let loaded = Loader::new(Settings::bare(&cwd, &home)) // the defaults: bare, or the host's own
///     .xdg_dirs(&env)     // the dirs under $XDG_DATA_HOME, $XDG_CONFIG_HOME
///     .user_file()        // <config dir>/config.toml; .file(path) for one the host names
///     .project_file()     // the project's .haest/config.toml, inside trusted_dirs only
///     .profile()          // the profile `profile` names
///     .env(env)           // HAEST_* variables (and the provider keys), as pairs or a map
///     .flags(flags)       // --model, -e, ...
///     .load(&fs)
///     .await?;
/// # let s = &loaded.config.settings;
/// # assert!(s.agents && s.tools.has("read") && !s.tools.has("bash"));
/// # assert_eq!(s.model_spec, "a:flag");
/// # Ok(())
/// # }
/// ```
struct Snippets;
