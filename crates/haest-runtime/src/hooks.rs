//! The runtime's hooks ([`RuntimeHooks`]): the one object the loop, the
//! ledger and the task board report to. It persists every record to the
//! session file, tells the human through the [`Ui`], answers the per-model
//! questions from the catalog, and compacts the conversation before a model
//! call that would overflow (compact.rs).

mod compact;

use std::{
    collections::HashSet,
    fmt,
    sync::{Arc, Mutex, MutexGuard},
};

use async_trait::async_trait;
use haest_settings::Settings;
use tokio_util::sync::CancellationToken;

pub(crate) use self::compact::Compact;
use self::compact::compaction_failed;
use crate::{
    Ui,
    haest::{
        Agent, AgentHooks, Effort,
        core::caps::{TurnCaps, TurnCapsSource},
        core::catalog::{InputModality, ModelFacts, Reasoning},
        core::compaction::needs_compaction,
        core::error::BoxError,
        core::ledger::{LedgerHooks, LedgerSnapshot},
        core::messages::MediaPart,
        core::remedy::Remedies,
        core::session::{JsonlSessionFile, SessionRecord, SessionSink},
        core::tool::ToolServices,
        now_ms,
        providers::Catalog,
    },
};

/// The runtime's hooks: what the agent loop, the ledger, the task list and
/// the tools report to.
///
/// It is the loop's [`AgentHooks`]: every message goes to the session file,
/// a tool with side effects runs only once the answer that called it is
/// written there, and a model call that would overflow the window is
/// compacted first. It is
/// the ledger's [`LedgerHooks`] (the spend goes to the session file) and the
/// tools' [`ToolServices`] (their log lines reach [`Ui::debug`] with `debug` on).
/// It answers the per-model questions (window, caps, input kinds, effort
/// levels) from the settings and the built-in catalog, and keeps the model and
/// effort the conversation is on now, which a subagent starts on.
///
/// [`build`](crate::build) makes one, read on the runtime as
/// [`Runtime::hooks`](crate::Runtime::hooks). A cheap `Clone`: every clone is
/// the same hooks, so a host that hands the runtime to a session keeps one to
/// tell the human through, or to flush the session file at exit.
#[derive(Clone)]
pub struct RuntimeHooks(Arc<Shared>);

/// What every clone of a [`RuntimeHooks`] shares.
struct Shared {
    /// The run's settings, as handed in.
    settings: Settings,
    /// The host's UI.
    ui: Arc<dyn Ui>,
    /// The session file records go to now; /clear swaps it.
    store: Mutex<Arc<JsonlSessionFile>>,
    /// The model the system prompt names and a subagent starts on; kept in step
    /// with the agent's spec.
    model_spec: Mutex<String>,
    /// The effort a subagent starts on; kept in step with the agent's.
    effort: Mutex<Option<Effort>>,
    /// The models the human was told once that they do not take an effort.
    effort_notes: Mutex<HashSet<String>>,
}

/// Every lock here is held only while this file's own code runs (never the
/// UI, never a write), so a poisoned one is a bug here.
fn held<T>(lock: &Mutex<T>) -> MutexGuard<'_, T> {
    lock.lock().expect("a runtime hooks lock poisoned")
}

impl RuntimeHooks {
    /// Hooks on `settings` that tell the human through `ui` and write to the
    /// session file `store`, starting on the settings' model and effort.
    ///
    /// Create `store`'s file first ([`JsonlSessionFile::create`] writes its
    /// header): a record appended before that lands in a file with no header,
    /// or nowhere. [`build`](crate::build) sees to that. Needs a tokio
    /// runtime, as the session file does.
    pub(crate) fn new(settings: Settings, ui: Arc<dyn Ui>, store: JsonlSessionFile) -> Self {
        let (model_spec, effort) = (settings.model_spec.clone(), settings.effort);
        Self(Arc::new(Shared {
            settings,
            ui,
            store: Mutex::new(Arc::new(store)),
            model_spec: Mutex::new(model_spec),
            effort: Mutex::new(effort),
            effort_notes: Mutex::default(),
        }))
    }

    /// The run's settings, as handed in. Their `model_spec` and `effort` are
    /// what the run started from: the ones in use now are
    /// [`model_spec`](Self::model_spec) and [`effort`](Self::effort).
    pub fn settings(&self) -> &Settings {
        &self.0.settings
    }

    /// The host's UI.
    pub(crate) fn ui(&self) -> &Arc<dyn Ui> {
        &self.0.ui
    }

    /// The UI's [`remedy`](Ui::remedy) for every limit, as the loop, the
    /// ledger, the inbox's gate and the sandbox take it.
    pub(crate) fn remedies(&self) -> Remedies {
        let ui = self.0.ui.clone();
        Remedies::new(move |limit| ui.remedy(limit))
    }

    /// The session file records go to now. [`Runtime::clear`](crate::Runtime::clear)
    /// swaps it, so ask again rather than keep it.
    pub(crate) fn store(&self) -> Arc<JsonlSessionFile> {
        held(&self.0.store).clone()
    }

    /// A fresh session file (/clear); records go there from now on.
    pub(crate) fn swap_store(&self, store: JsonlSessionFile) {
        *held(&self.0.store) = Arc::new(store);
    }

    /// Queue `record` for the session file. It returns at once: the write runs
    /// in order on the file's own task, and a failed one goes to
    /// [`Ui::error`].
    pub fn append(&self, record: &SessionRecord) {
        self.store().append(record);
    }

    /// Wait until every record queued so far is written. A host awaits it
    /// after each turn and before it exits.
    pub async fn flush(&self) {
        self.store().flush().await;
    }

    /// The model spec the conversation is on now: what the system prompt names
    /// and a subagent starts on.
    pub fn model_spec(&self) -> String {
        held(&self.0.model_spec).clone()
    }

    pub(crate) fn set_model_spec(&self, spec: &str) {
        *held(&self.0.model_spec) = spec.to_string();
    }

    /// The effort the conversation is on now (None = the model's default):
    /// what a subagent starts on.
    pub fn effort(&self) -> Option<Effort> {
        *held(&self.0.effort)
    }

    pub(crate) fn set_effort(&self, effort: Option<Effort>) {
        *held(&self.0.effort) = effort;
    }

    /// [`Ui::info`] on the host's UI.
    pub fn info(&self, text: &str) {
        self.0.ui.info(text);
    }

    /// [`Ui::warn`] on the host's UI.
    pub fn warn(&self, text: &str) {
        self.0.ui.warn(text);
    }

    /// [`Ui::error`] on the host's UI.
    pub fn error(&self, text: &str) {
        self.0.ui.error(text);
    }

    /// The window of the model `spec`, in tokens: the `context_window`
    /// setting, else the built-in catalog's, else 200K.
    pub(crate) fn window(&self, spec: &str) -> u64 {
        let catalog = Catalog::builtin().model(spec);
        self.0.settings.window(catalog.map(|m| m.context_window))
    }
}

/// The session file, the model and effort the conversation is on now.
impl fmt::Debug for RuntimeHooks {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RuntimeHooks")
            .field("store", &self.store())
            .field("model_spec", &self.model_spec())
            .field("effort", &self.effort())
            .finish_non_exhaustive()
    }
}

impl ToolServices for RuntimeHooks {
    fn debug(&self, message: &str) {
        if self.0.settings.debug {
            self.0.ui.debug(message);
        }
    }
}

#[async_trait]
impl AgentHooks for RuntimeHooks {
    fn on_message(&self, msg: &crate::haest::Message) {
        self.append(&SessionRecord::Message {
            t: now_ms(),
            message: msg.clone(),
        });
    }

    /// A full window is summarised first. A summary that fails ends the turn:
    /// tried again before every call it would cost two summary calls a step.
    async fn before_model_call(
        &self,
        agent: &mut Agent,
        cancel: &CancellationToken,
    ) -> Result<(), BoxError> {
        if needs_compaction(agent.history(), &self.compaction_for(agent.model_spec())) {
            let done = self.compact_now(agent, cancel, Compact::Auto).await;
            done.map_err(|e| compaction_failed(&e))?;
        }
        Ok(())
    }

    /// The answer that asked for the calls is written before any of them
    /// runs, so a resume after a crash knows they may have run.
    async fn before_effects(&self) {
        self.flush().await;
    }

    fn on_media_withheld(&self, parts: &[MediaPart], model: &str) {
        self.warn(&format!(
            "{} attachment(s) withheld: {model} cannot read them.",
            parts.len()
        ));
    }

    fn on_effort_fitted(&self, model: &str, used: Effort) {
        let key = format!("{model}:{}", used.as_str());
        if held(&self.0.effort_notes).insert(key) {
            self.info(&format!(
                "{model} does not take that effort; sent {}.",
                used.as_str()
            ));
        }
    }

    /// A turn's warning threshold: the human hears it as the ledger's.
    fn on_warning(&self, text: &str) {
        RuntimeHooks::warn(self, text);
    }
}

/// The token caps follow the model's window.
impl TurnCapsSource for RuntimeHooks {
    fn turn_caps(&self, spec: &str) -> Option<TurnCaps> {
        Some(self.0.settings.turn_caps_for(self.window(spec)))
    }
}

/// The inputs and reasoning are the built-in catalog's rows.
impl ModelFacts for RuntimeHooks {
    fn input_modalities(&self, spec: &str) -> Option<HashSet<InputModality>> {
        Catalog::builtin().input_modalities(spec)
    }

    fn reasoning(&self, spec: &str) -> Option<Reasoning> {
        Catalog::builtin().reasoning(spec).cloned()
    }
}

impl LedgerHooks for RuntimeHooks {
    fn persist(&self, snapshot: &LedgerSnapshot) {
        self.append(&SessionRecord::Ledger {
            t: now_ms(),
            ledger: snapshot.clone(),
        });
    }

    fn warn(&self, text: &str) {
        RuntimeHooks::warn(self, text);
    }
}
