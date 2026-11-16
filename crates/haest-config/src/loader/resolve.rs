//! Reading a [`Loader`](super::Loader)'s layers in the order they depend on
//! each other, and folding them into the settings.

use std::path::{Path, PathBuf};

use haest_core::fs::{Fs, normalize};
use haest_settings::Settings;

use crate::{
    ConfigError,
    fields::Config,
    file, handed, profiles,
    source::{ProjectFile, Resolved},
    stack::{Pending, Slot, Stack},
    value::{absolute, split_list},
};

/// The layers in their order of reading: what is in hand (variables, flags),
/// the files (from the config dir those give), the project's file (by the
/// trust those give), the profile (that all of them name).
pub(super) async fn resolve(
    defaults: Settings,
    mut stack: Stack,
    fs: &dyn Fs,
) -> Result<(Config, Resolved, Vec<String>), ConfigError> {
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

/// The nearest `.haest/config.toml`: read into the project layer when its
/// project is trusted, skipped when not.
async fn read_project(
    fs: &dyn Fs,
    cwd: &Path,
    home: &Path,
    trusted: &dyn Fn(&Path) -> bool,
    stack: &mut Stack,
) -> Result<ProjectFile, ConfigError> {
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
