//! A conversation's mod files as one mod ([`ModFiles`]): found in its mod
//! directories ([`ModFileOptions`], options.rs), each loaded as a
//! [`JsMod`], all of them held in one [`Mods`] that every event is handed
//! to, reloaded as they change (reload.rs).

mod options;
mod reload;

use std::sync::{Mutex, MutexGuard, PoisonError};

use async_trait::async_trait;
use haest_core::{
    Agent, AssistantMessage, Tools, TurnOutcome,
    error::BoxError,
    fs::FsRef,
    tool::{ToolContext, ToolOutput},
};
use haest_mods::{Before, Mod, ModEnv, ModToolCall, Mods, Step, TurnEnd};
#[cfg(doc)]
use haest_settings::ModReload;
use tokio_util::sync::CancellationToken;

#[cfg(doc)]
use super::JsMod;
pub use options::ModFileOptions;
use options::mod_files;
use reload::{Files, Look};

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
/// # Reloading
///
/// With [`ModFileOptions::with_reload`] at `ask` or `auto` ([`ModReload`]),
/// it reads every file again at each step boundary, as `model.before`
/// fires (before each model call, so after the host's own hook), through
/// the same `fs`: no watcher, nothing between steps. What changed since
/// the last look:
///
/// | On disk | `auto` | `ask` |
/// |---|---|---|
/// | a new or changed file | loads | loads on the human's Allow |
/// | a deleted file | unloads | unloads on the human's Allow |
/// | a file that fails to load | the version before runs; reported once | the same, after the Allow |
/// | a file that cannot be read | the version before runs; reported once | the same |
///
/// The ask goes through [`ModEnv::ask`] (the host's approval prompt), with
/// the new content whole as its preview. A Deny keeps the version that runs
/// and is never asked again for that content (or that deletion); an ask the
/// turn's interrupt ended is asked again at the next step. The files swap as
/// one: the model's next request already has a loaded file's tools and
/// hides what it hides, and no longer has a deleted one's. A file back to
/// the content that runs is no change. Native Rust mods never reload.
///
/// # Examples
///
/// ```
/// use haest_bridges::{ModFileOptions, ModFiles};
/// use haest_core::{MemFs, workspace::WorkspaceMode};
/// use haest_mods::{ModEnv, ModOptions, load_mods};
/// # use haest_mods::{ApprovalDecision, ApprovalRequest, ModUi};
/// # struct Terminal;
/// # #[async_trait::async_trait]
/// # impl ModUi for Terminal {
/// #     async fn ask(&self, _: ApprovalRequest, _: &tokio_util::sync::CancellationToken) -> ApprovalDecision { ApprovalDecision::Deny }
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
    /// The versions that run, as every event sees them; swapped whole.
    current: Mutex<Mods>,
    /// What is known of each file, held across a look (and its asks).
    files: tokio::sync::Mutex<Files>,
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
        let dirs = opts.mod_dirs(env.cwd());
        let mut files = Files::new(dirs, opts.caps, fs.into(), env, opts.reload);
        files.look(Look::Start).await;
        Self {
            current: Mutex::new(files.mods()),
            files: tokio::sync::Mutex::new(files),
        }
    }

    /// A step boundary: when the files reload, act on what changed.
    async fn reload(&self, cancel: &CancellationToken) {
        let mut files = self.files.lock().await;
        if files.reloads() && files.look(Look::Step(cancel)).await {
            *self.held() = files.mods();
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

/// Every event goes to the files as they are now ([`Mods`]' own `Mod`);
/// `model.before`, the step boundary, reloads them first.
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
        self.reload(cancel).await;
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
mod reload_tests;
#[cfg(test)]
mod tests;
