//! The session file a run writes: which one (new, named, or the latest in the
//! cwd), opened with its write errors going to the [`Ui`], started with a
//! header, and read back into place on a resume.

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
        session::{JsonlSessionFile, SessionRecord},
        task_board::TaskBoard,
    },
    sessions::{latest_in, new_session_id, sessions_dir},
};

/// The session file `id` under `data_dir`; a failed write is the [`Ui`]'s error.
pub(crate) fn open_store(
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

/// The session this run writes, opened: a new one (`resume` None), the latest
/// in the cwd (`Some("")`), or the one named. True when it resumes.
pub(crate) async fn open_session(
    fs: &Arc<dyn Fs>,
    s: &Settings,
    resume: Option<&str>,
    ui: Arc<dyn Ui>,
) -> anyhow::Result<(String, Arc<JsonlSessionFile>, bool)> {
    let (id, resuming) = match resume {
        None => (new_session_id(), false),
        Some("") => latest_in(fs.as_ref(), &sessions_dir(&s.data_dir), &s.cwd)
            .await
            .map(|id| (id, true))
            .ok_or_else(|| anyhow!("no saved session in {} to resume", s.cwd.display()))?,
        Some(id) => (id.to_string(), true),
    };
    let store = Arc::new(open_store(fs, &s.data_dir, &id, ui)?);
    if resuming && !store.exists().await {
        bail!("no saved session \"{id}\" (kern sessions lists them)");
    }
    Ok((id, store, resuming))
}

/// A new session file: header, the model and effort it starts on, the ledger so far.
pub(crate) fn init_store(host: &Host, agent: &Agent, id: &str) {
    host.store()
        .init(id, &host.settings.cwd.to_string_lossy(), now_ms());
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
pub(crate) async fn restore(
    host: &Host,
    ledger: &Mutex<Ledger>,
    board: &TaskBoard,
    opts: &mut AgentOptions,
) -> anyhow::Result<()> {
    let saved = host.store().load().await.context("reading the session")?;
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
    Ok(())
}
