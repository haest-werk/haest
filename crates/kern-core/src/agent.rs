//! The agent loop. One `run()` = one human turn: model call, run the tools it
//! asked for, feed results back, repeat until the model answers without tools.
//!
//! Invariants the loop keeps so every provider accepts the history:
//!   - every tool call gets exactly one tool result, even on interrupt;
//!   - input that arrives mid-turn (`AgentControl::steer`) is queued and
//!     injected at the next step boundary (after the tool results), never dropped;
//!   - the turn guard is asked before every model call, and its nudge rides in
//!     at the same step boundary.
//!
//! PORT (T3): `run`, `sendable`, `request_prefix` from core/loop.ts.

use std::sync::{Arc, Mutex};

use tokio_util::sync::CancellationToken;

use crate::{
    deps::AgentDeps,
    media::fit_for_model,
    messages::Message,
    provider::Effort,
    request_prefix::{RequestPrefix, build_request_prefix},
};

/// Steer or stop a running turn from another task. Cheap to clone.
#[derive(Clone, Default)]
pub struct AgentControl {
    inner: Arc<ControlInner>,
}

#[derive(Default)]
struct ControlInner {
    queue: Mutex<Vec<String>>,
    cancel: Mutex<Option<CancellationToken>>,
}

impl AgentControl {
    /// Queue input for the running turn; it lands at the next step boundary.
    pub fn steer(&self, text: impl Into<String>) {
        self.inner.queue.lock().unwrap().push(text.into());
    }

    /// Cancel the running turn, if any.
    pub fn abort(&self) {
        if let Some(c) = self.inner.cancel.lock().unwrap().as_ref() {
            c.cancel();
        }
    }

    pub fn running(&self) -> bool {
        self.inner.cancel.lock().unwrap().is_some()
    }

    /// Drain queued steering input, joined; the caller runs it as the next turn.
    pub fn take_queued(&self) -> Option<String> {
        let mut q = self.inner.queue.lock().unwrap();
        if q.is_empty() {
            return None;
        }
        Some(std::mem::take(&mut *q).join("\n\n"))
    }

    /// Mark a turn as running; Err when one already is.
    #[allow(dead_code)] // T3: run() calls it; remove this allow then
    pub(crate) fn begin(&self) -> Result<CancellationToken, String> {
        let mut c = self.inner.cancel.lock().unwrap();
        if c.is_some() {
            return Err("a turn is already running; use steer()".into());
        }
        let token = CancellationToken::new();
        *c = Some(token.clone());
        Ok(token)
    }

    #[allow(dead_code)] // T3: run() calls it; remove this allow then
    pub(crate) fn end(&self) {
        *self.inner.cancel.lock().unwrap() = None;
    }
}

pub struct AgentOptions {
    pub model: String,
    pub effort: Option<Effort>,
    pub history: Vec<Message>,
}

impl AgentOptions {
    pub fn new(model: impl Into<String>) -> Self {
        Self {
            model: model.into(),
            effort: None,
            history: Vec::new(),
        }
    }
}

pub struct Agent {
    pub messages: Vec<Message>,
    pub model_spec: String,
    pub effort: Option<Effort>,
    pub(crate) deps: AgentDeps,
    pub(crate) control: AgentControl,
}

impl Agent {
    pub fn new(deps: AgentDeps, opts: AgentOptions) -> Self {
        Self {
            messages: opts.history,
            model_spec: opts.model,
            effort: opts.effort,
            deps,
            control: AgentControl::default(),
        }
    }

    pub fn deps(&self) -> &AgentDeps {
        &self.deps
    }

    /// A handle to steer or abort this agent's turns from elsewhere.
    pub fn control(&self) -> AgentControl {
        self.control.clone()
    }

    /// Replace history wholesale (compaction, /clear). Does not fire `on_message`.
    pub fn replace_history(&mut self, messages: Vec<Message>) {
        self.messages = messages;
    }

    /// Everything a model call sends ahead of the history; compaction reuses it.
    pub async fn request_prefix(&self, model: &str) -> RequestPrefix {
        build_request_prefix(&self.deps, &self.model_spec, model, self.effort).await
    }

    /// History as the current model may receive it (media it cannot take is withheld, with a note).
    pub fn sendable(&self, messages: &[Message], model: &str) -> Vec<Message> {
        let hooks = self.deps.hooks.as_ref();
        let accepts = hooks.input_modalities(&self.model_spec);
        fit_for_model(messages, accepts.as_ref(), model, hooks)
    }
}
