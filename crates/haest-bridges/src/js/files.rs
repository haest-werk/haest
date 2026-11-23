//! A conversation's mod files as one mod ([`ModFiles`]): found in its mod
//! directories ([`ModFileOptions`]), each loaded as a [`JsMod`], all of them
//! held in one [`Mods`] that every event is handed to.

use std::{
    path::{Path, PathBuf},
    sync::{Mutex, MutexGuard, PoisonError},
};

use async_trait::async_trait;
use haest_core::{
    Agent, AssistantMessage, Tools, TurnOutcome,
    error::{BoxError, report},
    fs::{Fs, FsRef},
    tool::{ToolContext, ToolOutput},
};
use haest_mods::{Before, Mod, ModEnv, ModRef, ModToolCall, Mods, Step, TurnEnd};
use tokio_util::sync::CancellationToken;

use super::{JsMod, ModCaps};

/// Where a conversation's mod files are, and the bounds they run in.
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
    caps: ModCaps,
}

impl ModFileOptions {
    /// No mod directory, the default [`ModCaps`], for a host whose own
    /// config directory is `config_dir`.
    pub fn new(config_dir: impl Into<PathBuf>) -> Self {
        Self {
            config_dir: config_dir.into(),
            user_mods: false,
            project_mods: false,
            caps: ModCaps::default(),
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

    /// The bounds every mod file runs in, in place of [`ModCaps::default`].
    #[must_use]
    pub fn with_caps(mut self, caps: ModCaps) -> Self {
        self.caps = caps;
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
async fn mod_files(fs: &dyn Fs, dir: &Path) -> Vec<PathBuf> {
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

/// A conversation's `.js` / `.ts` mod files, as ONE mod: a host adds it
/// with [`ModOptions::with_mod`](haest_mods::ModOptions::with_mod), first
/// among its own, so the files run after the built-ins and before the
/// host's other mods.
///
/// It holds the files as a [`Mods`] and hands that every event, so the
/// files run among themselves by the rules any mods run by (in load order,
/// the first deny wins, a failure is reported under the file's own name).
/// Their tools, hidden tools and commands are the files' own, as the files
/// left them.
///
/// Each directory of [`ModFileOptions::mod_dirs`] loads in name order: its
/// `.ts`, `.js` and `.mjs` files (not `.d.ts`; subdirectories are not
/// searched), each running its default export ([`JsMod::load_with`], within
/// [`ModFileOptions::with_caps`]). A file that fails to load is reported
/// through [`ModEnv::error`] and skipped; the rest still load.
///
/// # Examples
///
/// ```
/// use haest_bridges::{ModFileOptions, ModFiles};
/// use haest_core::{MemFs, workspace::WorkspaceMode};
/// use haest_mods::{ModEnv, ModOptions, load_mods};
/// # use haest_mods::{ApprovalRequest, Decision, ModUi};
/// # struct Terminal;
/// # #[async_trait::async_trait]
/// # impl ModUi for Terminal {
/// #     async fn ask(&self, _: ApprovalRequest, _: &tokio_util::sync::CancellationToken) -> Decision { Decision::Deny }
/// #     fn info(&self, _: &str) {}
/// # }
///
/// # #[tokio::main(flavor = "current_thread")]
/// # async fn main() {
/// // The human's own mod file, in the host's config directory.
/// let disk = MemFs::from_files([(
///     "/home/me/.config/haest/mods/short.js",
///     "export default (haest) => haest.on('system.prompt', () => 'Answer in one line.')",
/// )]);
/// let env = ModEnv::new("/project", WorkspaceMode::Real, Terminal);
/// let opts = ModFileOptions::new("/home/me/.config/haest").with_user_mods(true);
/// let files = ModFiles::load(opts, disk, env).await;
/// assert_eq!(files.names(), ["/home/me/.config/haest/mods/short.js"]);
///
/// let mods = load_mods(ModOptions::new("/project", WorkspaceMode::Real).with_mod(files), Terminal);
/// assert_eq!(mods.names(), ["approval", "plan", "mod files"]);
/// assert_eq!(mods.system_append().await, "Answer in one line.");
/// # }
/// ```
pub struct ModFiles {
    current: Mutex<Mods>,
}

impl std::fmt::Debug for ModFiles {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ModFiles")
            .field("files", &self.names())
            .finish_non_exhaustive()
    }
}

impl ModFiles {
    /// The mod files `opts` names, read through `fs`, loaded for the
    /// conversation `env` is (its directory, its workspace, its human).
    ///
    /// `fs` is the HOST's own files (the real disk), never the agent's
    /// workspace: a mod file the agent wrote in its sandbox must not load.
    /// Never fails: the files that loaded are the ones it holds. Must be
    /// called inside a tokio runtime, or every file fails to load.
    pub async fn load(opts: ModFileOptions, fs: impl Into<FsRef>, env: ModEnv) -> Self {
        let fs = fs.into();
        let mut loaded = Vec::new();
        for dir in opts.mod_dirs(env.cwd()) {
            for path in mod_files(&*fs, &dir).await {
                match load_one(&*fs, &path, &env, opts.caps).await {
                    Ok(m) => loaded.push(ModRef::new(m)),
                    Err(why) => env.error(&format!(
                        "Mod {} failed to load and was skipped: {why}",
                        path.display()
                    )),
                }
            }
        }
        Self {
            current: Mutex::new(Mods::new(env, loaded)),
        }
    }

    /// The files loaded, by path, in the order they run.
    pub fn names(&self) -> Vec<String> {
        self.held().names()
    }

    /// The files as they are now.
    fn mods(&self) -> Mods {
        self.held().clone()
    }

    fn held(&self) -> MutexGuard<'_, Mods> {
        self.current.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// The file at `path`, read and loaded; why not, in the human's words.
async fn load_one(fs: &dyn Fs, path: &Path, env: &ModEnv, caps: ModCaps) -> Result<JsMod, String> {
    let source = fs.read_to_string(path).await.map_err(|e| e.to_string())?;
    JsMod::load_with(path, source, env, caps)
        .await
        .map_err(|e| report(&e))
}

/// Every event goes to the files as they are now ([`Mods`]' own `Mod`).
#[async_trait]
#[warn(clippy::missing_trait_methods)]
impl Mod for ModFiles {
    fn name(&self) -> &str {
        "mod files"
    }

    /// The files never judge a call after the others.
    fn is_gate(&self) -> bool {
        false
    }

    async fn tool_before(
        &self,
        call: &ModToolCall,
        ctx: &ToolContext,
        haest: &ModEnv,
    ) -> Result<Before, BoxError> {
        Mod::tool_before(&self.mods(), call, ctx, haest).await
    }

    async fn tool_after(
        &self,
        call: &ModToolCall,
        output: &ToolOutput,
        ctx: &ToolContext,
        haest: &ModEnv,
    ) -> Result<Option<ToolOutput>, BoxError> {
        Mod::tool_after(&self.mods(), call, output, ctx, haest).await
    }

    async fn prompt_submit(&self, text: &str, haest: &ModEnv) -> Result<Option<String>, BoxError> {
        Mod::prompt_submit(&self.mods(), text, haest).await
    }

    async fn system_prompt(&self, haest: &ModEnv) -> Result<Option<String>, BoxError> {
        Mod::system_prompt(&self.mods(), haest).await
    }

    async fn model_before(
        &self,
        agent: &mut Agent,
        cancel: &CancellationToken,
        haest: &ModEnv,
    ) -> Result<Step, BoxError> {
        Mod::model_before(&self.mods(), agent, cancel, haest).await
    }

    async fn model_after(
        &self,
        agent: &mut Agent,
        answer: &AssistantMessage,
        cancel: &CancellationToken,
        haest: &ModEnv,
    ) -> Result<Step, BoxError> {
        Mod::model_after(&self.mods(), agent, answer, cancel, haest).await
    }

    async fn turn_end(
        &self,
        agent: &mut Agent,
        outcome: &TurnOutcome,
        cancel: &CancellationToken,
        haest: &ModEnv,
    ) -> Result<TurnEnd, BoxError> {
        Mod::turn_end(&self.mods(), agent, outcome, cancel, haest).await
    }

    fn tools(&self) -> Tools {
        Mod::tools(&self.mods())
    }

    fn disabled_tools(&self) -> Vec<String> {
        Mod::disabled_tools(&self.mods())
    }

    fn commands(&self) -> Vec<String> {
        Mod::commands(&self.mods())
    }

    async fn command(
        &self,
        name: &str,
        args: &str,
        haest: &ModEnv,
    ) -> Result<Option<String>, BoxError> {
        Mod::command(&self.mods(), name, args, haest).await
    }
}

#[cfg(test)]
mod tests;
