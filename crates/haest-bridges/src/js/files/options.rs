//! Where a conversation's mod files are ([`ModFileOptions`]), and which
//! files in a directory are mods ([`mod_files`]).

use std::path::{Path, PathBuf};

use haest_code::VmCaps;
use haest_core::fs::Fs;
use haest_settings::ModReload;

/// Where a conversation's mod files are, the bounds they run in, and
/// whether they reload as they change.
///
/// [`ModFileOptions::new`] reads no directory: a host turns on the human's
/// own mods, and the project's only in a directory the human trusts.
///
/// ```
/// use std::path::{Path, PathBuf};
///
/// use haest_bridges::ModFileOptions;
///
/// let opts = ModFileOptions::new("/home/me/.config/haest").with_user_mods(true);
/// let dirs = opts.mod_dirs(Path::new("/project"));
/// assert_eq!(dirs, [PathBuf::from("/home/me/.config/haest/mods")]);
/// ```
#[derive(Debug, Clone)]
pub struct ModFileOptions {
    config_dir: PathBuf,
    user_mods: bool,
    project_mods: bool,
    pub(super) caps: VmCaps,
    pub(super) reload: ModReload,
}

impl ModFileOptions {
    /// No mod directory, a mod file's caps ([`VmCaps::mod_file`]), no
    /// reload, for a host whose own config directory is `config_dir`.
    pub fn new(config_dir: impl Into<PathBuf>) -> Self {
        Self {
            config_dir: config_dir.into(),
            user_mods: false,
            project_mods: false,
            caps: VmCaps::mod_file(),
            reload: ModReload::Off,
        }
    }

    /// Whether to load the human's own mods, `<config_dir>/mods`. Off by
    /// default: a host asks for them.
    #[must_use]
    pub fn with_user_mods(mut self, on: bool) -> Self {
        self.user_mods = on;
        self
    }

    /// Whether to load the project's mods, `<cwd>/.haest/mods`. Off by
    /// default: a repository's mod is that repository's code running in the
    /// host with full trust, and cloning a repository must never be enough
    /// to run it. Turn it on only for a directory the human trusts.
    #[must_use]
    pub fn with_project_mods(mut self, on: bool) -> Self {
        self.project_mods = on;
        self
    }

    /// The bounds every mod file runs in, in place of [`VmCaps::mod_file`].
    #[must_use]
    pub fn with_caps(mut self, caps: VmCaps) -> Self {
        self.caps = caps;
        self
    }

    /// Whether the files reload as they change, at each step boundary
    /// ([`ModReload`]; [`ModFiles`' reloading](super::ModFiles#reloading)).
    /// Off by default.
    #[must_use]
    pub fn with_reload(mut self, reload: ModReload) -> Self {
        self.reload = reload;
        self
    }

    /// Where mod files are looked for in a conversation in `cwd`, in load
    /// order: `<config_dir>/mods` with
    /// [`with_user_mods`](Self::with_user_mods), then `<cwd>/.haest/mods`
    /// with [`with_project_mods`](Self::with_project_mods). Empty by
    /// default.
    pub fn mod_dirs(&self, cwd: &Path) -> Vec<PathBuf> {
        let mut dirs = Vec::new();
        if self.user_mods {
            dirs.push(self.config_dir.join("mods"));
        }
        if self.project_mods {
            dirs.push(cwd.join(".haest").join("mods"));
        }
        dirs
    }
}

fn is_mod_file(name: &str) -> bool {
    let code = [".ts", ".js", ".mjs"].iter().any(|ext| name.ends_with(ext));
    code && !name.ends_with(".d.ts")
}

/// The mod files in `dir`, sorted by name: every `.ts`, `.js` and `.mjs`
/// file, but not a `.d.ts` declaration file. None when `dir` cannot be read
/// (it does not exist); subdirectories are not searched.
pub(super) async fn mod_files(fs: &dyn Fs, dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = fs.read_dir(dir).await else {
        return Vec::new();
    };
    let mut names: Vec<String> = entries
        .into_iter()
        .map(|e| e.name)
        .filter(|n| is_mod_file(n))
        .collect();
    names.sort();
    names.into_iter().map(|n| dir.join(n)).collect()
}
