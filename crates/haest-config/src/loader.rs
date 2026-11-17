//! [`Loader`]: haest's settings built from layers, in the order a host adds
//! them. The layers are read in resolve.rs.

mod resolve;

use std::{collections::HashMap, fmt, path::PathBuf};

use haest_core::fs::Fs;
use haest_providers::load_custom_providers;
use haest_settings::Settings;

use self::resolve::resolve;
use crate::{
    ConfigError, Loaded, handed,
    registry::{DEFS, flag_name},
    source::Source,
    stack::{Given, Pending, Slot, Stack},
    value::env_var,
};

/// A builder that reads haest's settings, layer over layer, when it is
/// [`load`](Loader::load)ed.
///
/// Each layer's values win over those of the layers added before it, the
/// defaults under them all. A layer not added is not read: a host that adds no
/// [`Loader::env`] reads no variable. Nothing is read until [`Loader::load`].
/// The crate docs list the layers [`Loader::standard`] adds.
///
/// Whatever order the host adds them in, the loader reads the variables and
/// flags first (they may name the config dir and the trusted dirs), then the
/// config files, then the project's file (by the trust those give), then the
/// profile (which any of them may name), and lays them in the order added.
///
/// # Examples
///
/// The `haest` command's layers, written out:
///
/// ```
/// # use std::collections::HashMap;
/// # use haest_config::Loader;
/// # use haest_settings::Settings;
/// # async fn host(fs: &dyn haest_core::fs::Fs, env: HashMap<String, String>, flags: Vec<(String, String)>) -> Result<(), haest_config::ConfigError> {
/// let loaded = Loader::new(Settings::bare("/work", "/home/me"))
///     .xdg_dirs(&env) // the default dirs under $XDG_DATA_HOME, $XDG_CONFIG_HOME
///     .user_file() // <config dir>/config.toml
///     .project_file() // the project's .haest/config.toml, inside trusted_dirs only
///     .profile() // the profile `profile` names
///     .env(env) // HAEST_* variables
///     .flags(flags) // --model, -e, ...
///     .load(fs)
///     .await?;
/// # Ok(()) }
/// ```
///
/// A host with a config file of its own and flags, but no variables and no
/// project files:
///
/// ```
/// use haest_config::Loader;
/// use haest_core::fs::MemFs;
/// use haest_settings::{Settings, WordSet};
///
/// # #[tokio::main(flavor = "current_thread")]
/// # async fn main() -> Result<(), haest_config::ConfigError> {
/// let fs = MemFs::from_files([("/etc/myhost/haest.toml", "tools = [\"read\"]\nmodel = \"a:file\"")]);
/// let loaded = Loader::new(Settings::bare("/srv/app", "/root"))
///     .file("/etc/myhost/haest.toml")
///     .flags([("model", "a:flag")])
///     .load(&fs)
///     .await?;
/// assert_eq!(loaded.config.settings.tools.words(), ["read"]);
/// assert_eq!(loaded.config.settings.model_spec, "a:flag");
/// assert!(loaded.keys.is_empty(), "no .env(): no variables, no keys");
/// # Ok(())
/// # }
/// ```
///
/// Its Debug output shows the layers' settings and the variables' names,
/// never a value: a variable may be a key.
#[must_use = "a Loader reads nothing until .load()"]
#[derive(Clone)]
pub struct Loader {
    defaults: Settings,
    stack: Stack,
    env: HashMap<String, String>,
}

impl fmt::Debug for Loader {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut names: Vec<&str> = self.env.keys().map(String::as_str).collect();
        names.sort_unstable();
        f.debug_struct("Loader")
            .field("defaults", &self.defaults)
            .field("layers", &self.stack)
            .field("env", &names)
            .finish()
    }
}

impl Loader {
    /// A loader with no layers yet, starting from these settings:
    /// [`Settings::bare`], or the host's own. Their `cwd` and `home` are where
    /// the project file is looked for from, and what `x` and `~/x` in a path
    /// setting are under; their `config_dir` is where
    /// [`user_file`](Self::user_file) looks unless a variable or a flag says
    /// otherwise.
    pub fn new(defaults: Settings) -> Self {
        Self {
            defaults,
            stack: Stack::default(),
            env: HashMap::new(),
        }
    }

    /// A loader with the layers the `haest` command reads, in its order:
    /// [`Loader::new`] then [`xdg_dirs`](Self::xdg_dirs),
    /// [`user_file`](Self::user_file), [`project_file`](Self::project_file),
    /// [`profile`](Self::profile), [`env`](Self::env), [`flags`](Self::flags).
    ///
    /// `env` is the process's environment (its variables, the provider keys
    /// among them), read once for both layers that take it; `flags` are
    /// `(setting, raw)` pairs as [`flags`](Self::flags) takes them. Each is
    /// any list of pairs: a `HashMap`, a `Vec`, an array, of `String`s or
    /// `&str`s. The crate docs have an example.
    pub fn standard<K, V, F, R>(
        defaults: Settings,
        env: impl IntoIterator<Item = (K, V)>,
        flags: impl IntoIterator<Item = (F, R)>,
    ) -> Self
    where
        K: Into<String>,
        V: Into<String>,
        F: Into<String>,
        R: Into<String>,
    {
        let env = owned(env);
        Self::new(defaults)
            .xdg_dirs(&env)
            .user_file()
            .project_file()
            .profile()
            .env(env)
            .flags(flags)
    }

    /// Adds a layer that makes `$XDG_DATA_HOME/haest` the `data_dir` and
    /// `$XDG_CONFIG_HOME/haest` the `config_dir`, each when that variable is
    /// set and not empty in `env` (any list of name, value pairs, borrowed:
    /// only the two values are read). Their source is the variable's name.
    pub fn xdg_dirs<K, V>(self, env: impl IntoIterator<Item = (K, V)>) -> Self
    where
        K: AsRef<str>,
        V: AsRef<str>,
    {
        const DIRS: [(&str, &str); 2] = [
            ("XDG_DATA_HOME", "data_dir"),
            ("XDG_CONFIG_HOME", "config_dir"),
        ];
        // A name given twice keeps the later value, as in a map.
        let mut values: [Option<String>; 2] = Default::default();
        for (var, value) in env {
            if let Some(i) = DIRS.iter().position(|(name, _)| *name == var.as_ref()) {
                values[i] = Some(value.as_ref().trim().to_string());
            }
        }
        let given = DIRS
            .into_iter()
            .zip(values)
            .filter_map(|((var, key), value)| {
                let value = value.filter(|v| !v.is_empty())?;
                let dir = PathBuf::from(value).join("haest");
                Some(Given::new(
                    key,
                    dir.display().to_string(),
                    Source::Env(var.into()),
                ))
            });
        self.add(Pending::Values(given.collect()))
    }

    /// Adds the layer `<config dir>/config.toml`, when there is one. The config
    /// dir is the `config_dir` the variables and flags give, else the
    /// defaults'. No file there reads nothing, quietly.
    pub fn user_file(self) -> Self {
        self.add(Pending::File(None))
    }

    /// Adds the layer of a config file the host names. A relative path is
    /// under the defaults' `cwd`, `~/` under their `home`. It must be there: a
    /// path with a typo in it must not silently read nothing. It may set what
    /// your own `config.toml` may, `trusted_dirs` included.
    pub fn file(self, path: impl Into<PathBuf>) -> Self {
        self.add(Pending::File(Some(path.into())))
    }

    /// Adds the layer of the nearest `.haest/config.toml` from `cwd` up (not
    /// the home dir's own), read only when its project is inside
    /// `trusted_dirs`, which a project's file can't set. An untrusted one is
    /// skipped with a note ([`Loaded::notes`]).
    pub fn project_file(self) -> Self {
        self.add(Pending::Project)
    }

    /// Adds the layer of the profile `profile` names, in any layer: the
    /// built-in one (`claude`), each file's `[profiles.<name>]` table over it.
    /// Without this layer a `profile` that is set is not applied, and a note
    /// says so.
    pub fn profile(self) -> Self {
        self.add(Pending::Profile)
    }

    /// Adds a layer of the `HAEST_*` variables in `env` (any list of name,
    /// value pairs: the process's environment as a `HashMap`, a fixed array;
    /// an empty value counts as unset). `env` is also what [`Loaded`] takes
    /// the provider keys, a child process's environment and every `${NAME}`
    /// from: without `.env()` those are empty. A second `.env()` adds to the
    /// first, a name in both keeping the later value.
    pub fn env<K, V>(mut self, env: impl IntoIterator<Item = (K, V)>) -> Self
    where
        K: Into<String>,
        V: Into<String>,
    {
        let env = owned(env);
        let given: Vec<Given> = DEFS
            .iter()
            .filter_map(|def| {
                let var = def.env();
                let raw = env_var(&env, &var)?;
                Some(Given::new(def.key, raw, Source::Env(var)))
            })
            .collect();
        self.env.extend(env);
        self.add(Pending::Values(given))
    }

    /// Adds a layer of flags, `(setting, raw)` each (`("max_turn_usd", "5")`),
    /// in the order given: a setting given twice keeps the later value, except
    /// `env`, where each adds a variable. The setting is the TOML name, not the
    /// flag's spelling; an error names the flag (`--max-turn-usd`).
    pub fn flags<K, V>(self, flags: impl IntoIterator<Item = (K, V)>) -> Self
    where
        K: Into<String>,
        V: Into<String>,
    {
        let given = flags.into_iter().map(|(key, raw)| {
            let key: String = key.into();
            let source = Source::Flag(flag_name(&key));
            Given::new(key, raw, source)
        });
        self.add(Pending::Values(given.collect()))
    }

    fn add(mut self, layer: Pending) -> Self {
        self.stack.0.push(Slot::Pending(layer));
        self
    }

    /// Read every layer through `fs` and lay each over the ones before it; then
    /// read what the runtime is handed beside the settings.
    ///
    /// Beside the settings it reads `<config dir>/providers.json`, a trusted
    /// project's `.mcp.json` (with `mcp` on and the `mcp` feature), and parses `provider_routing`. It
    /// turns `project_mods` off outside a trusted dir. What is asked for and
    /// not honoured is a note in [`Loaded::notes`], not an error.
    ///
    /// # Errors
    ///
    /// A [`ConfigError`] naming where the problem is, when: a setting is
    /// unknown ([`UnknownSetting`](ConfigError::UnknownSetting)); its value
    /// is not one it takes, a project's file sets what only you may
    /// (`trusted_dirs`, `provider_routing`), a profile sets `profile`,
    /// `trusted_dirs` or `provider_routing`, or a file sets `config_dir`
    /// (only a variable or a flag may) (all [`Invalid`](ConfigError::Invalid),
    /// each naming the setting in `key`); a file the host named with
    /// [`file`](Self::file) is not there; a config file cannot be read or is
    /// not TOML; `profile` names a profile there is none of; `providers.json`
    /// is malformed.
    pub async fn load(self, fs: &dyn Fs) -> Result<Loaded, ConfigError> {
        let Loader {
            defaults,
            stack,
            env,
        } = self;
        let (mut config, resolved, mut notes) = resolve(defaults, stack, fs).await?;
        let cwd = config.settings.cwd.clone();
        let project_mods = &mut config.settings.project_mods;
        handed::gate_project_mods(fs, &cwd, project_mods, resolved.trusted, &mut notes).await;
        #[cfg(feature = "mcp")]
        let mcp_servers =
            handed::mcp_servers(fs, &cwd, config.mcp, resolved.trusted, &env, &mut notes).await;
        #[cfg(not(feature = "mcp"))]
        handed::no_mcp(config.mcp, &mut notes);
        let providers = load_custom_providers(fs, &config.settings.config_dir).await?;
        let routing = config.provider_routing.as_deref();
        Ok(Loaded {
            child_env: handed::child_env(config.env_passthrough, &env),
            passed_env: handed::passed_env(&config.env, &env, &mut notes),
            routing: handed::routing(routing, &resolved.source("provider_routing"), &mut notes),
            keys: env.into(),
            config,
            resolved,
            providers,
            #[cfg(feature = "mcp")]
            mcp_servers,
            notes,
        })
    }
}

/// Name, value pairs as the map a layer reads them from.
fn owned<K: Into<String>, V: Into<String>>(
    pairs: impl IntoIterator<Item = (K, V)>,
) -> HashMap<String, String> {
    pairs
        .into_iter()
        .map(|(k, v)| (k.into(), v.into()))
        .collect()
}
