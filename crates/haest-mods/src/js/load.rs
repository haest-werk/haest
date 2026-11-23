//! Loading one file mod ([`JsMod::load`], [`JsMod::load_with`]): the bounds it
//! runs in ([`ModCaps`]) and why it did not load ([`ModLoadError`]).

use std::{
    io,
    path::Path,
    sync::{Arc, Mutex},
    time::Duration,
};

use haest_core::LiveTools;
use serde_json::json;

use super::{
    JsMod,
    channel::{CallError, JsMeta, JsThread},
    thread::{self, Setup},
};
use crate::{env::ModEnv, events::ModEvent};

/// The bounds a file mod runs in. The default: 10 s of code without a pause,
/// 128 MiB of memory, 30 s for its default export to return.
///
/// ```
/// use std::time::Duration;
///
/// use haest_mods::ModCaps;
///
/// let strict = ModCaps { load_timeout: Duration::from_secs(5), ..ModCaps::default() };
/// assert_eq!(strict.memory_bytes, 128 << 20);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModCaps {
    /// The longest the mod's code may run without pausing (awaiting does not
    /// count). Past it the code is stopped: every call into the mod in flight
    /// fails, and the next call runs as usual.
    pub slice: Duration,
    /// The mod's QuickJS memory, in bytes.
    pub memory_bytes: usize,
    /// How long the default export has to return when the mod loads.
    pub load_timeout: Duration,
}

impl Default for ModCaps {
    fn default() -> Self {
        Self {
            slice: Duration::from_secs(10),
            memory_bytes: 128 << 20,
            load_timeout: Duration::from_secs(30),
        }
    }
}

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
    /// Its default export did not return within [`ModCaps::load_timeout`].
    #[error("its default export did not return within {}s", .0.as_secs_f64())]
    Timeout(Duration),
}

impl JsMod {
    /// Runs one mod file within the default [`ModCaps`]: strips its types
    /// (a `.ts` path, through `haest-ts`), evaluates `source` as an ES module
    /// in a fresh QuickJS runtime on its own thread, and calls its default
    /// export with the `haest` API. `path` names the mod
    /// ([`Mod::name`](crate::Mod::name)) and its errors; the file is not
    /// read, `source` is the code.
    ///
    /// What the default export registers (events, tools, commands) is in
    /// place when this returns.
    ///
    /// [`load_mods`](crate::load_mods) calls this for every file in the mod
    /// directories; a host calls it to load one file of its own, then puts
    /// it in with [`ModOptions::with_mod`](crate::ModOptions::with_mod) or
    /// [`Mods::new`](crate::Mods::new).
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
    /// # use haest_mods::{ApprovalRequest, Decision, ModUi};
    /// use haest_mods::{JsMod, Mod, ModEnv, ModLoadError};
    /// # struct Terminal;
    /// # #[async_trait::async_trait]
    /// # impl ModUi for Terminal {
    /// #     async fn ask(&self, _: ApprovalRequest, _: &tokio_util::sync::CancellationToken) -> Decision { Decision::Deny }
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
        Self::load_with(path, source, haest, ModCaps::default()).await
    }

    /// [`load`](Self::load) within these `caps`.
    ///
    /// # Errors
    ///
    /// As [`load`](Self::load); [`ModLoadError::Timeout`] after
    /// `caps.load_timeout`.
    pub async fn load_with(
        path: &Path,
        source: String,
        haest: &ModEnv,
        caps: ModCaps,
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
        let thread = Arc::new(JsThread::new(thread::start(setup).await?));
        // The events are the Rust side's list: the JavaScript half takes
        // `haest.on` for these, and refuses to start if it handles others.
        let env = json!({
            "env": { "cwd": haest.cwd(), "workspace": haest.workspace().as_str() },
            "events": ModEvent::WORDS,
        });
        let wait = caps.load_timeout;
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
