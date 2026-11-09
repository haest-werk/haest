//! The CLI as a kern host: the one object the loop, the ledger and the task
//! board report to. It persists every record to the session file, prints what
//! the human must hear, answers the per-model questions from the catalog, and
//! compacts the conversation before a model call that would overflow.

use std::{
    collections::HashSet,
    io::{Stderr, Stdout},
    sync::{Arc, Mutex},
};

use async_trait::async_trait;
use kern::{
    Agent, AgentHooks, Effort,
    catalog::{InputModality, Reasoning},
    compaction::{CompactionConfig, compact, needs_compaction},
    ledger::{LedgerHooks, LedgerSnapshot},
    limits::TurnLimits,
    messages::MediaPart,
    providers::Catalog,
    session::{JsonlSessionFile, SessionRecord, SessionSink},
    tool::ToolHost,
};
use tokio_util::sync::CancellationToken;

use crate::{config::Config, render::Renderer};

pub type Term = Renderer<Stdout, Stderr>;

/// Messages kept verbatim after a compaction summary (the TS kern's number).
const KEEP_RECENT: usize = 12;
const COMPACT_SHARE: f64 = 0.8;

pub struct Host {
    pub cfg: Config,
    pub catalog: &'static Catalog,
    pub term: Arc<Mutex<Term>>,
    store: Mutex<Arc<JsonlSessionFile>>,
    /// The model the system prompt names; kept in step with the agent's spec.
    model: Mutex<String>,
    effort_notes: Mutex<HashSet<String>>,
}

impl Host {
    pub fn new(cfg: Config, term: Arc<Mutex<Term>>, store: Arc<JsonlSessionFile>) -> Self {
        let model = cfg.model.clone();
        Self {
            cfg,
            catalog: Catalog::builtin(),
            term,
            store: Mutex::new(store),
            model: Mutex::new(model),
            effort_notes: Mutex::default(),
        }
    }

    pub fn store(&self) -> Arc<JsonlSessionFile> {
        self.store.lock().unwrap().clone()
    }

    /// A fresh session file (/clear); records go there from now on.
    pub fn swap_store(&self, store: Arc<JsonlSessionFile>) {
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

    pub fn set_model(&self, spec: &str) {
        *self.model.lock().unwrap() = spec.to_string();
    }

    pub fn info(&self, text: &str) {
        self.term.lock().unwrap().info(text);
    }

    pub fn warn(&self, text: &str) {
        self.term.lock().unwrap().warn(text);
    }

    pub fn error(&self, text: &str) {
        self.term.lock().unwrap().error(text);
    }

    /// A question answered on the next line (see `ask.rs`).
    pub fn ask(&self, question: &str) {
        self.term.lock().unwrap().ask(question);
    }

    /// The model's window: KERN_CONTEXT_WINDOW, else the catalog's, else 200K.
    pub fn window(&self, spec: &str) -> u64 {
        self.cfg
            .window(self.catalog.model(spec).map(|m| m.context_window))
    }

    pub fn compaction_for(&self, spec: &str) -> CompactionConfig {
        CompactionConfig {
            context_window: self.window(spec),
            threshold: COMPACT_SHARE,
            ceiling: (self.cfg.compact_at > 0).then_some(self.cfg.compact_at),
            keep_recent: KEEP_RECENT,
        }
    }

    /// Compact now. `auto`: the loop's own check, so nothing to cut stays silent.
    pub async fn compact_now(&self, agent: &mut Agent, cancel: &CancellationToken, auto: bool) {
        let cfg = self.compaction_for(&agent.model_spec);
        match compact(agent, cancel, &cfg).await {
            Ok(None) if !auto => self.info("Nothing to compact yet."),
            Ok(None) => {}
            Ok(Some(res)) => {
                if let Some(ledger) = &agent.deps().ledger {
                    ledger.lock().unwrap().charge(&res.usage);
                }
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
            Err(e) => self.error(&format!("Compaction failed: {e}")),
        }
    }
}

pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

impl ToolHost for Host {
    fn log(&self, message: &str) {
        if self.cfg.debug {
            self.term.lock().unwrap().info(&format!("[kern] {message}"));
        }
    }
}

#[async_trait]
impl AgentHooks for Host {
    fn on_message(&self, msg: &kern::Message) {
        self.append(SessionRecord::Message {
            t: now_ms(),
            message: msg.clone(),
        });
    }

    async fn before_model_call(&self, agent: &mut Agent, cancel: &CancellationToken) {
        if needs_compaction(&agent.messages, &self.compaction_for(&agent.model_spec)) {
            self.compact_now(agent, cancel, true).await;
        }
    }

    fn limits(&self, spec: &str) -> Option<TurnLimits> {
        Some(self.cfg.limits_for(self.window(spec)))
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

/// The cost line's session total.
pub fn session_usd(agent: &Agent) -> Option<f64> {
    let ledger = agent.deps().ledger.as_ref()?;
    Some(ledger.lock().unwrap().snapshot().usd)
}
