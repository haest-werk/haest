//! Loading one file mod ([`JsMod::load`], [`JsMod::load_with`]) within its
//! caps ([`VmCaps`], [`VmCaps::mod_file`] by default), and why it did not
//! load ([`ModLoadError`]).

use std::{
    io,
    path::Path,
    sync::{Arc, Mutex},
    time::Duration,
};

use haest_code::VmCaps;
use haest_core::LiveTools;
use haest_mods::{ModEnv, ModEvent};
use serde_json::json;

use super::{
    JsMod,
    channel::{CallError, JsMeta},
    thread::{self, Setup},
};

/// Why a mod file did not load ([`JsMod::load`]). Every case is the file's
/// own doing except [`NoRuntime`](Self::NoRuntime),
/// [`Thread`](Self::Thread) and [`Stopped`](Self::Stopped).
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ModLoadError {
    /// It was loaded outside a tokio runtime.
    #[error("file mods load inside a tokio runtime")]
    NoRuntime,
    /// Its thread could not start.
    #[error("its thread could not start")]
    Thread(#[source] io::Error),
    /// Its thread stopped (panicked) while loading.
    #[error("its thread stopped while loading")]
    Stopped,
    /// A `.ts` file that does not strip to JavaScript.
    #[error("it is not valid TypeScript")]
    TypeScript(#[source] haest_ts::StripError),
    /// QuickJS could not start, or the module did not evaluate: a syntax
    /// error, an `import`, a throw at its top level. The engine's own words.
    #[error("{0}")]
    Evaluate(String),
    /// Its default export is not a function.
    #[error("its default export is not a function")]
    NotAFunction,
    /// Its top-level `await` never finished.
    #[error("its top-level await never finished")]
    NeverFinished,
    /// Its default export threw, or registered what the `haest` API refuses.
    /// The engine's own words.
    #[error("{0}")]
    Export(String),
    /// Its default export did not return within [`VmCaps::timeout`].
    #[error("its default export did not return within {}s", .0.as_secs_f64())]
    Timeout(Duration),
}

impl JsMod {
    /// Runs one mod file within a mod file's caps ([`VmCaps::mod_file`]):
    /// strips its types (a `.ts` path, through `haest-ts`), evaluates
    /// `source` as an ES module on a fresh engine of its own (haest-code's,
    /// the one the Code tool runs on) on its own thread, and calls its
    /// default export with the `haest` API. `path` names the mod
    /// ([`Mod::name`](haest_mods::Mod::name)) and its errors; the file is not
    /// read, `source` is the code.
    ///
    /// What the default export registers (events, tools, commands) is in
    /// place when this returns.
    ///
    /// [`ModFiles`](crate::ModFiles) calls this for every file in the mod
    /// directories; a host calls it to load one file of its own, then puts
    /// it in with [`ModOptions::with_mod`](haest_mods::ModOptions::with_mod)
    /// or [`Mods::new`](haest_mods::Mods::new).
    ///
    /// # Errors
    ///
    /// A [`ModLoadError`]: no tokio runtime is running, the TypeScript or
    /// JavaScript does not parse, the module threw, its default export is not
    /// a function, its top-level `await` never finished, or the default
    /// export threw or did not return in time.
    ///
    /// # Examples
    ///
    /// ```
    /// # use std::path::Path;
    /// # use haest_core::workspace::WorkspaceMode;
    /// # use haest_mods::{ApprovalDecision, ApprovalRequest, ModUi};
    /// use haest_bridges::{JsMod, ModLoadError};
    /// use haest_mods::{Mod, ModEnv};
    /// # struct Terminal;
    /// # #[async_trait::async_trait]
    /// # impl ModUi for Terminal {
    /// #     async fn ask(&self, _: ApprovalRequest, _: &tokio_util::sync::CancellationToken) -> ApprovalDecision { ApprovalDecision::Deny }
    /// #     fn info(&self, _: &str) {}
    /// # }
    ///
    /// # #[tokio::main(flavor = "current_thread")]
    /// # async fn main() -> Result<(), haest_core::error::BoxError> {
    /// let env = ModEnv::new("/project", WorkspaceMode::Sandbox, Terminal);
    /// let source = "export default (haest: any) => haest.on('prompt.submit', (t: string) => t.trim())";
    /// let m = JsMod::load(Path::new("/mods/trim.ts"), source.into(), &env).await?;
    /// assert_eq!(m.prompt_submit("  hello  ", &env).await?.as_deref(), Some("hello"));
    ///
    /// let broken = JsMod::load(Path::new("/mods/broken.js"), "export default 42".into(), &env).await;
    /// assert!(matches!(broken, Err(ModLoadError::NotAFunction)));
    /// # Ok(())
    /// # }
    /// ```
    pub async fn load(path: &Path, source: String, haest: &ModEnv) -> Result<Self, ModLoadError> {
        Self::load_with(path, source, haest, VmCaps::mod_file()).await
    }

    /// [`load`](Self::load) within these `caps`: its memory, its
    /// [`slice`](VmCaps::slice), and its [`timeout`](VmCaps::timeout) for the
    /// default export to return. Build them from [`VmCaps::mod_file`], what
    /// `load` uses: [`VmCaps::default`] is the Code tool's caps (64 MiB, a
    /// minute), not a mod file's (128 MiB, 30 s).
    ///
    /// # Errors
    ///
    /// As [`load`](Self::load); [`ModLoadError::Timeout`] after
    /// `caps.timeout`.
    pub async fn load_with(
        path: &Path,
        source: String,
        haest: &ModEnv,
        caps: VmCaps,
    ) -> Result<Self, ModLoadError> {
        let runtime =
            tokio::runtime::Handle::try_current().map_err(|_no_runtime| ModLoadError::NoRuntime)?;
        let meta = Arc::new(Mutex::new(JsMeta::default()));
        let setup = Setup {
            path: path.to_path_buf(),
            source,
            ui: haest.ui().clone(),
            meta: meta.clone(),
            caps,
            runtime,
        };
        let thread = Arc::new(thread::start(setup).await?);
        // The events are the Rust side's list: the JavaScript half takes
        // `haest.on` for these, and refuses to start if it handles others.
        let env = json!({
            "env": { "cwd": haest.cwd(), "workspace": haest.workspace().as_str() },
            "events": ModEvent::WORDS,
        });
        let wait = caps.timeout;
        match tokio::time::timeout(wait, thread.call("load", env, None)).await {
            Err(_) => Err(ModLoadError::Timeout(wait)),
            Ok(Err(CallError::Failed(words))) => Err(ModLoadError::Export(words)),
            Ok(Err(CallError::Stopped | CallError::Interrupted)) => Err(ModLoadError::Stopped),
            Ok(Ok(_)) => Ok(Self {
                name: path.display().to_string(),
                thread,
                meta,
                tools: LiveTools::new(),
            }),
        }
    }
}
