//! The session file a run writes: which one (new, named, or the latest in the
//! cwd), opened with its write errors going to the [`Ui`], created with its
//! header before anything starts, and read back into place on a resume.

use std::{
    path::Path,
    sync::{Arc, Mutex},
};

use anyhow::{Context, anyhow, bail};
use kern_settings::Settings;

use crate::{
    Host, Ui,
    kern::{
        Agent, AgentOptions,
        fs::Fs,
        ledger::Ledger,
        now_ms,
        session::{JsonlSessionFile, ReplayedSession, SessionRecord, sessions_dir},
        task_board::TaskBoard,
    },
    sessions::{latest_in, new_session_id},
};

/// The session a run writes, opened and, on a resume, read. Nothing has
/// started yet, so an error here fails the build with nothing to undo.
pub(crate) struct OpenSession {
    pub store: Arc<JsonlSessionFile>,
    /// What a resumed session saved; None = a new session, its file not
    /// created yet ([`create`]).
    pub saved: Option<ReplayedSession>,
}

/// The session file `id` under `data_dir`; a failed write is the [`Ui`]'s error.
fn open_store(
    fs: &Arc<dyn Fs>,
    data_dir: &Path,
    id: &str,
    ui: Arc<dyn Ui>,
) -> anyhow::Result<JsonlSessionFile> {
    JsonlSessionFile::with_error_handler(fs.clone(), data_dir, id, move |e| {
        ui.error(&format!("session log write failed: {e}"))
    })
    .map_err(|e| anyhow!("{e}"))
}

/// The session this run writes: a new one (`resume` None), the latest in the
/// cwd (`Some("")`), or the one named.
pub(crate) async fn open_session(
    fs: &Arc<dyn Fs>,
    s: &Settings,
    resume: Option<&str>,
    ui: Arc<dyn Ui>,
) -> anyhow::Result<OpenSession> {
    let id = match resume {
        None => {
            let store = open_store(fs, &s.data_dir, &new_session_id(), ui)?;
            let store = Arc::new(store);
            return Ok(OpenSession { store, saved: None });
        }
        Some("") => latest_in(fs.as_ref(), &sessions_dir(&s.data_dir), &s.cwd)
            .await
            .ok_or_else(|| anyhow!("no saved session in {} to resume", s.cwd.display()))?,
        Some(id) => id.to_string(),
    };
    let store = open_store(fs, &s.data_dir, &id, ui)?;
    if !store.exists().await {
        bail!("no saved session \"{id}\" (kern sessions lists them)");
    }
    let saved = store.load().await.context("reading the session")?;
    Ok(OpenSession {
        store: Arc::new(store),
        saved: Some(saved),
    })
}

/// A new session's file, on disk with its header. Until it is, nothing of the
/// session has been written: a session that cannot be saved fails here.
pub(crate) async fn create(store: &JsonlSessionFile, cwd: &Path) -> anyhow::Result<()> {
    let id = store.session_id();
    store
        .create(id, &cwd.to_string_lossy(), now_ms())
        .await
        .with_context(|| format!("creating the session file {}", store.path().display()))
}

/// A new session with its file created: what /clear starts.
pub(crate) async fn new_session(
    fs: &Arc<dyn Fs>,
    s: &Settings,
    ui: Arc<dyn Ui>,
) -> anyhow::Result<JsonlSessionFile> {
    let store = open_store(fs, &s.data_dir, &new_session_id(), ui)?;
    create(&store, &s.cwd).await?;
    Ok(store)
}

/// What a new session starts on: the model, the effort, the ledger so far.
pub(crate) fn record_start(host: &Host, agent: &Agent) {
    host.append(SessionRecord::Config {
        t: now_ms(),
        model: Some(agent.model_spec.clone()),
        effort: Some(agent.effort),
    });
    if let Some(ledger) = &agent.deps().ledger {
        let snapshot = ledger.lock().unwrap().snapshot();
        host.append(SessionRecord::Ledger {
            t: now_ms(),
            ledger: snapshot,
        });
    }
}

/// A saved session back in place: its ledger, task list, model, effort and history.
pub(crate) fn restore(
    host: &Host,
    saved: ReplayedSession,
    ledger: &Mutex<Ledger>,
    board: &TaskBoard,
    opts: &mut AgentOptions,
) {
    if saved.corrupt_lines > 0 {
        host.warn(&format!(
            "Skipped {} torn line(s) in the session file.",
            saved.corrupt_lines
        ));
    }
    if let Some(l) = &saved.ledger {
        ledger.lock().unwrap().restore(l);
    }
    board.restore(saved.tasks.unwrap_or_default());
    if let Some(model) = saved.model {
        opts.model = model;
    }
    if let Some(effort) = saved.effort {
        opts.effort = effort;
    }
    opts.history = saved.messages;
}
