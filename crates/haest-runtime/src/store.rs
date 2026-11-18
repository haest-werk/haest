//! The session file a run writes: which one (new, named, or the latest in the
//! cwd), opened with its write errors going to the [`Ui`], held for this run
//! alone, created with its header before anything starts, and read back into
//! place on a resume.

use std::{io, path::Path, sync::Arc};

use haest_settings::Settings;

use crate::{
    BuildOptions, OnResume, Prefer, Resume, RuntimeHooks, SessionFileError, Ui,
    haest::{
        Agent, AgentOptions, Effort,
        core::error::report,
        core::fs::FsRef,
        core::ledger::Ledger,
        core::session::{
            EffortChange, JsonlSessionFile, ReplayedSession, SessionRecord, sessions_dir,
        },
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
) -> Result<JsonlSessionFile, SessionFileError> {
    let store = JsonlSessionFile::new_with_error_handler(fs.clone(), data_dir, id, move |e| {
        ui.error(&format!("session log write failed: {e}"));
    })?;
    Ok(store)
}

/// The session held for this run alone while the store lives, so no other
/// run writes it: `InUse` when another holds it. A lock the file system
/// cannot take must not cost the session: the run goes on unlocked, as runs
/// did before sessions were locked, and `ui` is told once that another run
/// could write the session too.
async fn hold(store: &JsonlSessionFile, ui: &dyn Ui) -> Result<(), SessionFileError> {
    match store.try_lock().await {
        Err(e) if e.kind() == io::ErrorKind::WouldBlock => Err(SessionFileError::InUse {
            id: store.session_id().to_string(),
        }),
        Err(e) => {
            ui.warn(&format!(
                "Session {} cannot be locked here ({}), so another run could write it too.",
                store.session_id(),
                report(&e)
            ));
            Ok(())
        }
        Ok(()) => Ok(()),
    }
}

/// The session this run writes, as `resume` says. A saved one is held for
/// this run before it is read (a new one before it is created, [`create`]).
pub(crate) async fn open_session(
    fs: &FsRef,
    s: &Settings,
    resume: &Resume,
    ui: Arc<dyn Ui>,
) -> Result<OpenSession, SessionFileError> {
    let id = match resume {
        Resume::New => {
            let store = open_store(fs, &s.data_dir, &new_session_id(), ui)?;
            return Ok(OpenSession { store, saved: None });
        }
        Resume::Latest => latest_in(fs.as_ref(), &sessions_dir(&s.data_dir), &s.cwd)
            .await
            .ok_or_else(|| SessionFileError::NoneToResume { cwd: s.cwd.clone() })?,
        Resume::Id(id) => id.clone(),
    };
    let store = open_store(fs, &s.data_dir, &id, ui.clone())?;
    if !store.exists().await {
        return Err(SessionFileError::NotFound { id });
    }
    hold(&store, &*ui).await?;
    let saved = store
        .load()
        .await
        .map_err(|source| SessionFileError::Read {
            path: store.path().to_path_buf(),
            source,
        })?;
    Ok(OpenSession {
        store,
        saved: Some(saved),
    })
}

/// A new session held for this run, then its file on disk with its header.
/// It is held first, so a session in use leaves no file behind. Until the
/// file is there, nothing of the session has been written: a session that
/// cannot be saved fails here.
pub(crate) async fn create(
    store: &JsonlSessionFile,
    cwd: &Path,
    ui: &dyn Ui,
) -> Result<(), SessionFileError> {
    hold(store, ui).await?;
    let id = store.session_id();
    store
        .create(id, cwd, now_ms())
        .await
        .map_err(|source| SessionFileError::Create {
            path: store.path().to_path_buf(),
            source,
        })
}

/// A new session with its file created: what /clear starts.
pub(crate) async fn new_session(
    fs: &FsRef,
    s: &Settings,
    ui: Arc<dyn Ui>,
) -> Result<JsonlSessionFile, SessionFileError> {
    let store = open_store(fs, &s.data_dir, &new_session_id(), ui.clone())?;
    create(&store, &s.cwd, &*ui).await?;
    Ok(store)
}

/// What a new session starts on: the model, the effort, the ledger so far.
pub(crate) fn record_start(hooks: &RuntimeHooks, agent: &Agent) {
    hooks.append(&SessionRecord::Config {
        t: now_ms(),
        model_spec: Some(agent.model_spec().to_string()),
        effort: agent.effort().into(),
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
    let saved_one = |prefer: Prefer| prefer == Prefer::Saved;
    let model = saved
        .and_then(|r| r.model_spec.clone())
        .filter(|_| saved_one(opts.on_resume.model));
    let effort = saved
        .map(|r| r.effort)
        .filter(|_| saved_one(opts.on_resume.effort))
        .unwrap_or_default();
    (
        model.unwrap_or_else(|| s.model_spec.clone()),
        effort.applied_to(s.effort),
    )
}

/// A resume on a model or effort this run asked for: recorded like a switch,
/// so the next resume goes on with it.
pub(crate) fn record_given(hooks: &RuntimeHooks, agent: &Agent, opts: &BuildOptions) {
    let OnResume { model, effort } = opts.on_resume;
    let (model, effort) = (model == Prefer::Settings, effort == Prefer::Settings);
    if model || effort {
        hooks.append(&SessionRecord::Config {
            t: now_ms(),
            model_spec: model.then(|| agent.model_spec().to_string()),
            effort: if effort {
                agent.effort().into()
            } else {
                EffortChange::Unchanged
            },
        });
    }
}

/// A saved session back in place: its ledger, task list and history (the
/// model and effort are [`start_on`]'s); `opts` with the history.
pub(crate) fn restore(
    hooks: &RuntimeHooks,
    saved: ReplayedSession,
    ledger: &Ledger,
    board: &TaskBoard,
    opts: AgentOptions,
) -> AgentOptions {
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
    opts.with_history(saved.messages)
}
