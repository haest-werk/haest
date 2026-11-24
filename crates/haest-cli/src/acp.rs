//! `haest acp`: haest as an Agent Client Protocol agent, an editor (Zed's
//! agent panel, JetBrains, any ACP client) driving it over stdin and stdout.
//! Each session the editor opens is a conversation over haest's own files
//! for the directory it names, loaded as `haest` would load them there, with
//! this command's flags and environment.

use std::{collections::HashMap, io::Write, path::PathBuf, process::ExitCode};

use async_trait::async_trait;
use haest::{
    acp::{AcpAgent, Conversations, Setup},
    config::{Loaded, Loader},
    core::error::{BoxError, report},
    runtime::RuntimeInputs,
    settings::Settings,
    workspace::RealFs,
};

/// haest's files, loaded for the directory each session names.
struct HaestFiles {
    home: String,
    env: HashMap<String, String>,
    /// The setting flags, `(key, raw)`: they win over the files there.
    flags: Vec<(String, String)>,
}

impl HaestFiles {
    async fn load(&self, cwd: PathBuf) -> Result<Loaded, haest::config::ConfigError> {
        let defaults = Settings::bare(cwd, &self.home);
        Loader::standard(defaults, self.env.clone(), self.flags.clone())
            .load(&RealFs)
            .await
    }
}

/// What the loader noted (a setting it skipped, a file it could not read) on
/// stderr: stdout is the protocol's, and an editor keeps stderr in its log.
fn note(loaded: &Loaded) {
    let mut err = std::io::stderr().lock();
    for n in &loaded.notes {
        let _ = writeln!(err, "haest: {}", crate::render::inert(n));
    }
}

#[async_trait]
impl Conversations for HaestFiles {
    async fn inputs(&self, setup: &Setup) -> Result<RuntimeInputs, BoxError> {
        let loaded = self.load(setup.cwd.clone()).await?;
        note(&loaded);
        Ok(haest::runtime_inputs(loaded))
    }
}

/// Serve the editor on stdin and stdout until it closes them. The flags and
/// files are loaded once first, for `cwd`, so a bad one fails here and not in
/// the editor's first session.
pub(crate) async fn serve(
    env: HashMap<String, String>,
    flags: Vec<(String, String)>,
    cwd: PathBuf,
    home: String,
) -> ExitCode {
    let files = HaestFiles { home, env, flags };
    if let Err(e) = files.load(cwd.clone()).await {
        return crate::fail(2, crate::load_failed(e));
    }
    match AcpAgent::new(files, cwd).serve_stdio().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => crate::fail(1, report(&e)),
    }
}
