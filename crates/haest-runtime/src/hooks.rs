//! The runtime's hooks ([`RuntimeHooks`]): the one object the loop, the
//! ledger and the task board report to. It persists every record to the
//! session file, tells the human through the [`Ui`], answers the per-model
//! questions from the catalog, and compacts the conversation before a model
//! call that would overflow.

use std::{
    collections::HashSet,
    sync::{Arc, Mutex},
};

use async_trait::async_trait;
use haest_settings::Settings;
use tokio_util::sync::CancellationToken;

use crate::{
    Ui,
    haest::{
        Agent, AgentHooks, Effort,
        core::caps::TurnCaps,
        core::catalog::{InputModality, Reasoning},
        core::compaction::{CompactionConfig, compact, needs_compaction},
        core::ledger::{LedgerHooks, LedgerSnapshot},
        core::messages::{MediaPart, Usage},
        core::remedy::Remedies,
        core::session::{JsonlSessionFile, SessionRecord, SessionSink},
        core::tool::ToolServices,
        now_ms,
        providers::Catalog,
    },
};

/// Messages kept verbatim after a compaction summary (the TS haest's number).
const KEEP_RECENT: usize = 12;
const COMPACT_SHARE: f64 = 0.8;

/// The runtime's hooks: what the agent loop, the ledger, the task list and
/// the tools report to.
///
/// It is the loop's [`AgentHooks`]: every message goes to the session file,
/// and a model call that would overflow the window is compacted first. It is
/// the ledger's [`LedgerHooks`] (the spend goes to the session file) and the
/// tools' [`ToolServices`] (their log lines reach [`Ui::debug`] with `debug` on).
/// It answers the per-model questions (window, limits, input kinds, effort
/// levels) from the settings and the built-in catalog, and keeps the model and
/// effort the conversation is on now, which a subagent starts on.
///
/// [`build`](crate::build) makes one, shared as [`Runtime::hooks`](crate::Runtime::hooks).
/// A host that takes the pieces makes its own with [`RuntimeHooks::new`].
pub struct RuntimeHooks {
    /// The run's settings, as handed in. Their `model` and `effort` are what
    /// the run started from: the ones in use now are [`model`](Self::model)
    /// and [`effort`](Self::effort).
    pub settings: Settings,
    /// haest's built-in model catalog: windows, input kinds, effort levels.
    pub catalog: &'static Catalog,
    /// The host's UI, as a trait object.
    pub ui: Arc<dyn Ui>,
    store: Mutex<Arc<JsonlSessionFile>>,
    /// The model the system prompt names and a subagent starts on; kept in step
    /// with the agent's spec.
    model: Mutex<String>,
    /// The effort a subagent starts on; kept in step with the agent's.
    effort: Mutex<Option<Effort>>,
    effort_notes: Mutex<HashSet<String>>,
}

impl RuntimeHooks {
    /// Hooks on `settings` that tell the human through `ui` and write to the
    /// session file `store`, starting on the settings' model and effort.
    ///
    /// Create `store`'s file first ([`JsonlSessionFile::create`] writes its
    /// header): a record appended before that lands in a file with no header,
    /// or nowhere. [`build`](crate::build) sees to that for its own. Needs a
    /// tokio runtime, as the session file does.
    pub fn new(settings: Settings, ui: Arc<dyn Ui>, store: Arc<JsonlSessionFile>) -> Self {
        let (model, effort) = (settings.model.clone(), settings.effort);
        Self {
            settings,
            catalog: Catalog::builtin(),
            ui,
            store: Mutex::new(store),
            model: Mutex::new(model),
            effort: Mutex::new(effort),
            effort_notes: Mutex::default(),
        }
    }

    /// The UI's [`remedy`](Ui::remedy) for every limit, as the loop, the
    /// ledger, the inbox's gate and the sandbox take it.
    pub fn remedies(&self) -> Remedies {
        let ui = self.ui.clone();
        Remedies::new(move |limit| ui.remedy(limit))
    }

    /// The session file records go to now. [`Runtime::clear`](crate::Runtime::clear)
    /// swaps it, so ask again rather than keep it.
    pub fn store(&self) -> Arc<JsonlSessionFile> {
        self.store.lock().unwrap().clone()
    }

    /// A fresh session file (/clear); records go there from now on.
    pub(crate) fn swap_store(&self, store: Arc<JsonlSessionFile>) {
        *self.store.lock().unwrap() = store;
    }

    /// Queue `record` for the session file. It returns at once: the write runs
    /// in order on the file's own task, and a failed one goes to
    /// [`Ui::error`].
    pub fn append(&self, record: SessionRecord) {
        self.store().append(&record);
    }

    /// Wait until every record queued so far is written. A host awaits it
    /// after each turn and before it exits.
    pub async fn flush(&self) {
        self.store().flush().await;
    }

    /// The model the conversation is on now: what the system prompt names and
    /// a subagent starts on.
    pub fn model(&self) -> String {
        self.model.lock().unwrap().clone()
    }

    pub(crate) fn set_model(&self, spec: &str) {
        *self.model.lock().unwrap() = spec.to_string();
    }

    /// The effort the conversation is on now (None = the model's default):
    /// what a subagent starts on.
    pub fn effort(&self) -> Option<Effort> {
        *self.effort.lock().unwrap()
    }

    pub(crate) fn set_effort(&self, effort: Option<Effort>) {
        *self.effort.lock().unwrap() = effort;
    }

    /// [`Ui::info`] on [`ui`](Self::ui).
    pub fn info(&self, text: &str) {
        self.ui.info(text);
    }

    /// [`Ui::warn`] on [`ui`](Self::ui).
    pub fn warn(&self, text: &str) {
        self.ui.warn(text);
    }

    /// [`Ui::error`] on [`ui`](Self::ui).
    pub fn error(&self, text: &str) {
        self.ui.error(text);
    }

    /// The window of the model `spec`, in tokens: the `context_window`
    /// setting, else the catalog's, else 200K.
    pub fn window(&self, spec: &str) -> u64 {
        self.settings
            .window(self.catalog.model(spec).map(|m| m.context_window))
    }

    /// When to compact a conversation on the model `spec`: at 80% of its
    /// [`window`](Self::window), or at the `compact_at` setting when that is
    /// lower and not 0. About the last 12 messages are kept as they are.
    pub fn compaction_for(&self, spec: &str) -> CompactionConfig {
        CompactionConfig {
            context_window: self.window(spec),
            threshold: COMPACT_SHARE,
            ceiling: Some(self.settings.limits.compact_at).filter(|n| *n > 0),
            keep_recent: KEEP_RECENT,
        }
    }

    /// Compact `agent`'s conversation now: the older messages become one
    /// summary, the latest are kept, and the session file records it. What
    /// the summary calls cost goes on the conversation's spend either way.
    ///
    /// `auto`: the loop's own check, so nothing to cut stays silent; a host's
    /// /compact passes false and the human hears "Nothing to compact yet."
    /// `cancel` stops the summary call.
    ///
    /// # Errors
    ///
    /// When no summary came out: the history is as it was. The error is said
    /// to the UI already, unless `cancel` fired: then the human stopped it,
    /// and the turn ends as aborted.
    pub async fn compact_now(
        &self,
        agent: &mut Agent,
        cancel: &CancellationToken,
        auto: bool,
    ) -> Result<(), String> {
        let cfg = self.compaction_for(agent.model_spec());
        match compact(agent, cancel, &cfg).await {
            Ok(None) if !auto => self.info("Nothing to compact yet."),
            Ok(None) => {}
            Ok(Some(res)) => {
                charge(agent, &res.usage);
                self.append(SessionRecord::Compaction {
                    t: now_ms(),
                    summary: res.summary,
                    kept: res.kept,
                });
                self.info(&format!(
                    "Conversation compacted; kept the last {} messages.",
                    res.kept
                ));
            }
            Err(e) => {
                charge(agent, &e.usage());
                let why = format!("Compaction failed: {e}");
                if !cancel.is_cancelled() {
                    self.error(&why);
                }
                return Err(why);
            }
        }
        Ok(())
    }
}

/// What the summary calls cost goes on the conversation's bill, summary or
/// not. The loop only counts it toward the turn's caps (`take_side_spend`).
fn charge(agent: &Agent, usage: &Usage) {
    if let Some(ledger) = &agent.deps().ledger {
        ledger.lock().unwrap().charge(usage);
    }
}

impl ToolServices for RuntimeHooks {
    fn log(&self, message: &str) {
        if self.settings.debug {
            self.ui.debug(message);
        }
    }
}

#[async_trait]
impl AgentHooks for RuntimeHooks {
    fn on_message(&self, msg: &crate::haest::Message) {
        self.append(SessionRecord::Message {
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
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        if needs_compaction(agent.history(), &self.compaction_for(agent.model_spec())) {
            self.compact_now(agent, cancel, true).await?;
        }
        Ok(())
    }

    fn turn_caps(&self, spec: &str) -> Option<TurnCaps> {
        Some(self.settings.turn_caps_for(self.window(spec)))
    }

    fn input_modalities(&self, spec: &str) -> Option<HashSet<InputModality>> {
        self.catalog.input_modalities(spec)
    }

    fn on_media_withheld(&self, parts: &[MediaPart], model: &str) {
        self.warn(&format!(
            "{} attachment(s) withheld: {model} cannot read them.",
            parts.len()
        ));
    }

    fn reasoning(&self, spec: &str) -> Option<Reasoning> {
        self.catalog.reasoning(spec).cloned()
    }

    fn on_effort_fitted(&self, model: &str, used: Effort) {
        let key = format!("{model}:{}", used.as_str());
        if self.effort_notes.lock().unwrap().insert(key) {
            self.info(&format!(
                "{model} does not take that effort; sent {}.",
                used.as_str()
            ));
        }
    }
}

impl LedgerHooks for RuntimeHooks {
    fn persist(&self, snapshot: &LedgerSnapshot) {
        self.append(SessionRecord::Ledger {
            t: now_ms(),
            ledger: snapshot.clone(),
        });
    }

    fn warn(&self, text: &str) {
        RuntimeHooks::warn(self, text);
    }
}

/// The conversation's spend so far, in US dollars, from `agent`'s ledger.
/// None when the agent has no ledger (one [`build`](crate::build) made always
/// has). A resumed session's spend includes what it cost before.
pub fn session_usd(agent: &Agent) -> Option<f64> {
    let ledger = agent.deps().ledger.as_ref()?;
    Some(ledger.lock().unwrap().snapshot().usd)
}
