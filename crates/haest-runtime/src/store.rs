//! The session file a run writes: which one (new, named, or the latest in the
//! cwd), opened with its write errors going to the [`Ui`], created with its
//! header before anything starts, and read back into place on a resume.

use std::{path::Path, sync::Arc};

use haest_settings::Settings;

use crate::{
    BuildOptions, Resume, RuntimeHooks, SessionError, Ui,
    haest::{
        Agent, AgentOptions, Effort,
        core::fs::FsRef,
        core::ledger::Ledger,
        core::session::{JsonlSessionFile, ReplayedSession, SessionRecord, sessions_dir},
        core::task_board::TaskBoard,
        now_ms,
    },
    sessions::{latest_in, new_session_id},
};

/// The session a run writes, opened and, on a resume, read. Nothing has
/// started yet, so an error here fails the build with nothing to undo.
pub(crate) struct OpenSession {
    pub store: JsonlSessionFile,
    /// What a resumed session saved; None = a new session, its file not
    /// created yet ([`create`]).
    pub saved: Option<ReplayedSession>,
}

/// The session file `id` under `data_dir`; a failed write is the [`Ui`]'s error.
fn open_store(
    fs: &FsRef,
    data_dir: &Path,
    id: &str,
    ui: Arc<dyn Ui>,
) -> Result<JsonlSessionFile, SessionError> {
    let store = JsonlSessionFile::with_error_handler(fs.clone(), data_dir, id, move |e| {
        ui.error(&format!("session log write failed: {e}"));
    })?;
    Ok(store)
}

/// The session this run writes, as `resume` says.
pub(crate) async fn open_session(
    fs: &FsRef,
    s: &Settings,
    resume: &Resume,
    ui: Arc<dyn Ui>,
) -> Result<OpenSession, SessionError> {
    let id = match resume {
        Resume::New => {
            let store = open_store(fs, &s.data_dir, &new_session_id(), ui)?;
            return Ok(OpenSession { store, saved: None });
        }
        Resume::Latest => latest_in(fs.as_ref(), &sessions_dir(&s.data_dir), &s.cwd)
            .await
            .ok_or_else(|| SessionError::NoneToResume { cwd: s.cwd.clone() })?,
        Resume::Id(id) => id.clone(),
    };
    let store = open_store(fs, &s.data_dir, &id, ui)?;
    if !store.exists().await {
        return Err(SessionError::NotFound { id });
    }
    let saved = store.load().await.map_err(|source| SessionError::Read {
        path: store.path().to_path_buf(),
        source,
    })?;
    Ok(OpenSession {
        store,
        saved: Some(saved),
    })
}

/// A new session's file, on disk with its header. Until it is, nothing of the
/// session has been written: a session that cannot be saved fails here.
pub(crate) async fn create(store: &JsonlSessionFile, cwd: &Path) -> Result<(), SessionError> {
    let id = store.session_id();
    store
        .create(id, cwd, now_ms())
        .await
        .map_err(|source| SessionError::Create {
            path: store.path().to_path_buf(),
            source,
        })
}

/// A new session with its file created: what /clear starts.
pub(crate) async fn new_session(
    fs: &FsRef,
    s: &Settings,
    ui: Arc<dyn Ui>,
) -> Result<JsonlSessionFile, SessionError> {
    let store = open_store(fs, &s.data_dir, &new_session_id(), ui)?;
    create(&store, &s.cwd).await?;
    Ok(store)
}

/// What a new session starts on: the model, the effort, the ledger so far.
pub(crate) fn record_start(hooks: &RuntimeHooks, agent: &Agent) {
    hooks.append(&SessionRecord::Config {
        t: now_ms(),
        model_spec: Some(agent.model_spec().to_string()),
        effort: Some(agent.effort()),
    });
    if let Some(ledger) = agent.deps().shared().ledger() {
        let snapshot = ledger.snapshot();
        hooks.append(&SessionRecord::Ledger {
            t: now_ms(),
            ledger: snapshot,
        });
    }
}

/// The model and effort the conversation starts on: the settings', except
/// that a resume goes on with the ones the session saved, unless the host
/// says this run asked for its own ([`BuildOptions`]).
pub(crate) fn start_on(
    s: &Settings,
    saved: Option<&ReplayedSession>,
    opts: &BuildOptions,
) -> (String, Option<Effort>) {
    let model = saved
        .and_then(|r| r.model_spec.clone())
        .filter(|_| !opts.model_spec_given);
    let effort = saved.and_then(|r| r.effort).filter(|_| !opts.effort_given);
    (
        model.unwrap_or_else(|| s.model_spec.clone()),
        effort.unwrap_or(s.effort),
    )
}

/// A resume on a model or effort this run asked for: recorded like a switch,
/// so the next resume goes on with it.
pub(crate) fn record_given(hooks: &RuntimeHooks, agent: &Agent, opts: &BuildOptions) {
    if opts.model_spec_given || opts.effort_given {
        hooks.append(&SessionRecord::Config {
            t: now_ms(),
            model_spec: opts
                .model_spec_given
                .then(|| agent.model_spec().to_string()),
            effort: opts.effort_given.then_some(agent.effort()),
        });
    }
}

/// A saved session back in place: its ledger, task list and history (the
/// model and effort are [`start_on`]'s).
pub(crate) fn restore(
    hooks: &RuntimeHooks,
    saved: ReplayedSession,
    ledger: &Ledger,
    board: &TaskBoard,
    opts: &mut AgentOptions,
) {
    if saved.corrupt_lines > 0 {
        hooks.warn(&format!(
            "Skipped {} torn line(s) in the session file.",
            saved.corrupt_lines
        ));
    }
    if let Some(l) = &saved.ledger {
        ledger.restore(l);
    }
    board.restore(saved.tasks.unwrap_or_default());
    opts.history = saved.messages;
}
