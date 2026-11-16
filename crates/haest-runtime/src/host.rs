//! A conversation's host: the one object the loop, the ledger and the task
//! board report to. It persists every record to the session file, tells the
//! human through the [`Ui`], answers the per-model questions from the catalog,
//! and compacts the conversation before a model call that would overflow.

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
        catalog::{InputModality, Reasoning},
        compaction::{CompactionConfig, compact, needs_compaction},
        ledger::{LedgerHooks, LedgerSnapshot},
        limits::TurnLimits,
        messages::{MediaPart, Usage},
        now_ms,
        providers::Catalog,
        session::{JsonlSessionFile, SessionRecord, SessionSink},
        tool::ToolHost,
    },
};

/// Messages kept verbatim after a compaction summary (the TS haest's number).
const KEEP_RECENT: usize = 12;
const COMPACT_SHARE: f64 = 0.8;

pub struct Host {
    pub settings: Settings,
    pub catalog: &'static Catalog,
    pub ui: Arc<dyn Ui>,
    store: Mutex<Arc<JsonlSessionFile>>,
    /// The model the system prompt names and a subagent starts on; kept in step
    /// with the agent's spec.
    model: Mutex<String>,
    /// The effort a subagent starts on; kept in step with the agent's.
    effort: Mutex<Option<Effort>>,
    effort_notes: Mutex<HashSet<String>>,
}

impl Host {
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

    pub fn store(&self) -> Arc<JsonlSessionFile> {
        self.store.lock().unwrap().clone()
    }

    /// A fresh session file (/clear); records go there from now on.
    pub(crate) fn swap_store(&self, store: Arc<JsonlSessionFile>) {
        *self.store.lock().unwrap() = store;
    }

    pub fn append(&self, record: SessionRecord) {
        self.store().append(&record);
    }

    pub async fn flush(&self) {
        self.store().flush().await;
    }

    pub fn model(&self) -> String {
        self.model.lock().unwrap().clone()
    }

    pub(crate) fn set_model(&self, spec: &str) {
        *self.model.lock().unwrap() = spec.to_string();
    }

    pub fn effort(&self) -> Option<Effort> {
        *self.effort.lock().unwrap()
    }

    pub(crate) fn set_effort(&self, effort: Option<Effort>) {
        *self.effort.lock().unwrap() = effort;
    }

    pub fn info(&self, text: &str) {
        self.ui.info(text);
    }

    pub fn warn(&self, text: &str) {
        self.ui.warn(text);
    }

    pub fn error(&self, text: &str) {
        self.ui.error(text);
    }

    /// The model's window: the `context_window` setting, else the catalog's, else 200K.
    pub fn window(&self, spec: &str) -> u64 {
        self.settings
            .window(self.catalog.model(spec).map(|m| m.context_window))
    }

    pub fn compaction_for(&self, spec: &str) -> CompactionConfig {
        CompactionConfig {
            context_window: self.window(spec),
            threshold: COMPACT_SHARE,
            ceiling: Some(self.settings.limits.compact_at).filter(|n| *n > 0),
            keep_recent: KEEP_RECENT,
        }
    }

    /// Compact now. `auto`: the loop's own check, so nothing to cut stays silent.
    /// `Err`: no summary; history is as it was. Said to the UI already, unless
    /// `cancel` fired: the human stopped it, and the turn ends as aborted.
    pub async fn compact_now(
        &self,
        agent: &mut Agent,
        cancel: &CancellationToken,
        auto: bool,
    ) -> Result<(), String> {
        let cfg = self.compaction_for(&agent.model_spec);
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

impl ToolHost for Host {
    fn log(&self, message: &str) {
        if self.settings.debug {
            self.ui.debug(message);
        }
    }
}

#[async_trait]
impl AgentHooks for Host {
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
    ) -> Result<(), String> {
        if needs_compaction(&agent.messages, &self.compaction_for(&agent.model_spec)) {
            self.compact_now(agent, cancel, true).await?;
        }
        Ok(())
    }

    fn limits(&self, spec: &str) -> Option<TurnLimits> {
        Some(self.settings.limits_for(self.window(spec)))
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

impl LedgerHooks for Host {
    fn persist(&self, snapshot: &LedgerSnapshot) {
        self.append(SessionRecord::Ledger {
            t: now_ms(),
            ledger: snapshot.clone(),
        });
    }

    fn warn(&self, text: &str) {
        Host::warn(self, text);
    }
}

/// The conversation's spend so far.
pub fn session_usd(agent: &Agent) -> Option<f64> {
    let ledger = agent.deps().ledger.as_ref()?;
    Some(ledger.lock().unwrap().snapshot().usd)
}
