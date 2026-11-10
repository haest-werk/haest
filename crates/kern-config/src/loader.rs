//! [`Loader`]: kern's settings built from layers, in the order a host adds them.

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};

use kern_core::fs::{Fs, normalize};
use kern_providers::load_custom_providers;
use kern_settings::Settings;

use crate::{
    Loaded,
    fields::Config,
    file, handed, profiles,
    registry::{DEFS, flag_name},
    source::{ProjectFile, Resolved, Source},
    stack::{Given, Pending, Slot, Stack},
    value::{absolute, env_var, split_list},
};

/// kern's settings, layer over layer: each layer's values win over those of
/// the layers added before it, the defaults under them all. A layer not added
/// is not read: a host that adds no [`Loader::env`] reads no variable. Nothing
/// is read until [`Loader::load`].
///
/// ```
/// # use std::collections::HashMap;
/// # use kern_config::Loader;
/// # use kern_settings::Settings;
/// # async fn host(fs: &dyn kern_core::fs::Fs, env: HashMap<String, String>, flags: Vec<(String, String)>) -> Result<(), String> {
/// let loaded = Loader::new(Settings::bare("/work", "/home/me"))
///     .xdg_dirs(&env) // the default dirs under $XDG_DATA_HOME, $XDG_CONFIG_HOME
///     .user_file() // <config dir>/config.toml
///     .project_file() // the project's .kern/config.toml, inside trusted_dirs only
///     .profile() // the profile `profile` names
///     .env(&env) // KERN_* variables
///     .flags(&flags) // --model, -e, ...
///     .load(fs)
///     .await?;
/// # Ok(()) }
/// ```
#[must_use = "a Loader reads nothing until .load()"]
#[derive(Debug)]
pub struct Loader {
    defaults: Settings,
    stack: Stack,
    env: HashMap<String, String>,
}

impl Loader {
    /// Start from these settings: [`Settings::bare`], or the host's own. Their
    /// `cwd` and `home` are where the project file is looked for from, and
    /// what `x` and `~/x` in a path setting are under.
    pub fn new(defaults: Settings) -> Self {
        Self {
            defaults,
            stack: Stack::default(),
            env: HashMap::new(),
        }
    }

    /// The layers the `kern` command reads, in its order: [`Loader::new`]
    /// then [`xdg_dirs`](Self::xdg_dirs), [`user_file`](Self::user_file),
    /// [`project_file`](Self::project_file), [`profile`](Self::profile),
    /// [`env`](Self::env), [`flags`](Self::flags).
    pub fn standard(
        defaults: Settings,
        env: &HashMap<String, String>,
        flags: &[(String, String)],
    ) -> Self {
        Self::new(defaults)
            .xdg_dirs(env)
            .user_file()
            .project_file()
            .profile()
            .env(env)
            .flags(flags)
    }

    /// The dirs under `$XDG_DATA_HOME/kern` and `$XDG_CONFIG_HOME/kern`, when set.
    pub fn xdg_dirs(self, env: &HashMap<String, String>) -> Self {
        let given = [
            ("XDG_DATA_HOME", "data_dir"),
            ("XDG_CONFIG_HOME", "config_dir"),
        ]
        .into_iter()
        .filter_map(|(var, key)| {
            let dir = PathBuf::from(env_var(env, var)?).join("kern");
            Some(Given::new(
                key,
                dir.display().to_string(),
                Source::Env(var.into()),
            ))
        });
        self.add(Pending::Values(given.collect()))
    }

    /// `<config dir>/config.toml`, when there is one. The config dir is the
    /// `config_dir` the variables and flags give, else the defaults'.
    pub fn user_file(self) -> Self {
        self.add(Pending::File(None))
    }

    /// A config file the host names. It must be there: a path with a typo in
    /// it must not silently read nothing.
    pub fn file(self, path: impl Into<PathBuf>) -> Self {
        self.add(Pending::File(Some(path.into())))
    }

    /// The nearest `.kern/config.toml` from `cwd` up (not the home dir's own),
    /// read only inside `trusted_dirs`, which a project's file can't set.
    pub fn project_file(self) -> Self {
        self.add(Pending::Project)
    }

    /// The profile `profile` names, in any layer: the built-in one (`claude`),
    /// each file's `[profiles.<name>]` table over it.
    pub fn profile(self) -> Self {
        self.add(Pending::Profile)
    }

    /// `KERN_*` variables. `env` is also what [`Loaded`] takes the provider
    /// keys and a child process's environment from: without `.env()` those
    /// are empty.
    pub fn env(mut self, env: &HashMap<String, String>) -> Self {
        self.env
            .extend(env.iter().map(|(k, v)| (k.clone(), v.clone())));
        let given = DEFS.iter().filter_map(|def| {
            let var = def.env();
            let raw = env_var(env, &var)?;
            Some(Given::new(def.key, raw, Source::Env(var)))
        });
        self.add(Pending::Values(given.collect()))
    }

    /// Flags, `(setting, raw)` each (`("max_turn_usd", "5")`), in the order given.
    pub fn flags(self, flags: &[(String, String)]) -> Self {
        let given = flags
            .iter()
            .map(|(key, raw)| Given::new(key, raw, Source::Flag(flag_name(key))));
        self.add(Pending::Values(given.collect()))
    }

    fn add(mut self, layer: Pending) -> Self {
        self.stack.0.push(Slot::Pending(layer));
        self
    }

    /// Read every layer and lay each over the ones before it; then read what
    /// the runtime is handed beside the settings.
    pub async fn load(self, fs: &dyn Fs) -> Result<Loaded, String> {
        let Loader {
            defaults,
            stack,
            env,
        } = self;
        let (mut config, resolved, mut notes) = resolve(defaults, stack, fs).await?;
        let cwd = config.settings.cwd.clone();
        let s = &mut config.settings;
        handed::gate_project_mods(fs, &cwd, &mut s.project_mods, resolved.trusted, &mut notes)
            .await;
        let mcp_servers =
            handed::mcp_servers(fs, &cwd, config.mcp, resolved.trusted, &env, &mut notes).await;
        let providers = load_custom_providers(fs, &config.settings.config_dir)
            .await
            .map_err(|e| e.0)?;
        let routing = config.provider_routing.as_deref();
        Ok(Loaded {
            child_env: handed::child_env(config.env_passthrough, &env),
            env: handed::passed_env(&config.env, &env, &mut notes),
            routing: handed::routing(routing, &resolved.source("provider_routing"), &mut notes),
            keys: env,
            config,
            resolved,
            providers,
            mcp_servers,
            notes,
        })
    }
}

/// The layers in their order of reading: what is in hand (variables, flags),
/// the files (from the config dir those give), the project's file (by the
/// trust those give), the profile (that all of them name).
async fn resolve(
    defaults: Settings,
    mut stack: Stack,
    fs: &dyn Fs,
) -> Result<(Config, Resolved, Vec<String>), String> {
    let (cwd, home) = (defaults.cwd.clone(), defaults.home.clone());
    let at = |path: &str| absolute(path, &cwd, &home);
    let mut notes = Vec::new();

    stack.read_values()?;
    let config_dir = stack
        .last("config_dir")
        .map_or_else(|| defaults.config_dir.clone(), at);
    for slot in &mut stack.0 {
        if let Slot::Pending(Pending::File(named)) = slot {
            let named = named.as_deref().map(|p| at(&p.to_string_lossy()));
            let read = file::read_user_file(fs, named, &config_dir).await?;
            *slot = Slot::Read(read.unwrap_or_default());
        }
    }

    let trusted_dirs: Vec<PathBuf> = stack
        .last("trusted_dirs")
        .map(split_list)
        .unwrap_or_default()
        .iter()
        .map(|dir| at(dir))
        .collect();
    let trusted = |dir: &Path| trusted_dirs.iter().any(|t| normalize(dir).starts_with(t));
    let project_file = if stack.has(&Pending::Project) {
        read_project(fs, &cwd, &home, &trusted, &mut stack).await?
    } else {
        ProjectFile::None
    };
    if let ProjectFile::Untrusted(path) = &project_file {
        notes.push(handed::untrusted(path, file::project_root(path)));
    }

    if let Some(name) = stack.last("profile").map(String::from) {
        if stack.has(&Pending::Profile) {
            let layer = profiles::layer(&name, &stack)?;
            stack.fill(&Pending::Profile, &layer);
        } else {
            notes.push(format!(
                "profile \"{name}\" is set, but this loader reads no profile (Loader::profile): not applied."
            ));
        }
    }

    let (config, sources) = stack.fold(Config::from(defaults))?;
    let resolved = Resolved {
        sources,
        project_file,
        trusted: trusted(&cwd),
    };
    Ok((config, resolved, notes))
}

/// The nearest `.kern/config.toml`: read into the project layer when its
/// project is trusted, skipped when not.
async fn read_project(
    fs: &dyn Fs,
    cwd: &Path,
    home: &Path,
    trusted: &dyn Fn(&Path) -> bool,
    stack: &mut Stack,
) -> Result<ProjectFile, String> {
    let Some(path) = file::find_project_file(fs, cwd, home).await else {
        return Ok(ProjectFile::None);
    };
    if !trusted(file::project_root(&path)) {
        return Ok(ProjectFile::Untrusted(path));
    }
    let layer = file::read_project_file(fs, &path).await?;
    stack.fill(&Pending::Project, &layer);
    Ok(ProjectFile::Read(path))
}
