//! The agent loop. One `run()` = one human turn: model call, run the tools it
//! asked for, feed results back, repeat until the model answers without tools.
//!
//! Invariants the loop keeps so every provider accepts the history:
//!   - every tool call gets exactly one tool result, even on interrupt;
//!   - input that arrives mid-turn (`AgentControl::steer`) is queued and
//!     injected at the next step boundary (after the tool results), never dropped;
//!   - the turn guard (guard.rs) is asked before every model call, and its
//!     nudge rides in at the same step boundary.

#[cfg(test)]
pub(crate) mod test_support;
mod turn;

use std::{
    sync::{Arc, Mutex},
    time::Instant,
};

use tokio_util::sync::CancellationToken;

use crate::{
    deps::AgentDeps,
    events::{EventSink, TurnEnd},
    guard::TurnGuard,
    limits::TurnLimits,
    media::fit_for_model,
    messages::{Message, StopReason, Usage},
    provider::Effort,
    request_prefix::{RequestPrefix, build_request_prefix},
};

use turn::Turn;

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
    pub(crate) fn begin(&self) -> Result<CancellationToken, String> {
        let mut c = self.inner.cancel.lock().unwrap();
        if c.is_some() {
            return Err("a turn is already running; use steer()".into());
        }
        let token = CancellationToken::new();
        *c = Some(token.clone());
        Ok(token)
    }

    pub(crate) fn end(&self) {
        *self.inner.cancel.lock().unwrap() = None;
    }
}

/// Ends the running turn however `run` stops: returning, or its future dropped.
/// Anything still queued after the turn ended is a fresh turn for the caller.
struct Running(AgentControl);

impl Drop for Running {
    fn drop(&mut self) {
        self.0.end();
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
    /// Model calls made for this conversation outside the loop's own (the
    /// compaction summary in a hook) since the running turn last looked.
    side_spend: Usage,
}

impl Agent {
    pub fn new(deps: AgentDeps, opts: AgentOptions) -> Self {
        Self {
            messages: opts.history,
            model_spec: opts.model,
            effort: opts.effort,
            deps,
            control: AgentControl::default(),
            side_spend: Usage::default(),
        }
    }

    /// A model call made for this conversation outside the loop (compaction's
    /// summary): it counts toward the running turn's caps before its next model call.
    pub(crate) fn spent_outside_the_loop(&mut self, usage: &Usage) {
        self.side_spend.add(usage);
    }

    /// What was spent outside the loop since the last look.
    pub(crate) fn take_side_spend(&mut self) -> Usage {
        std::mem::take(&mut self.side_spend)
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

    /// One human turn. Every event goes to `sink` in order, the last one is
    /// [`crate::LoopEvent::TurnEnd`]; the return value is how that turn ended.
    /// Input that arrives meanwhile goes through [`AgentControl::steer`]; stop it
    /// with [`AgentControl::abort`] -- dropping this future mid-turn instead can
    /// leave a tool call in history without its result.
    pub async fn run(&mut self, input: &str, sink: &mut dyn EventSink) -> TurnEnd {
        let started = Instant::now();
        let cancel = match self.control.begin() {
            Ok(c) => c,
            Err(e) => {
                let mut turn = Turn::new(CancellationToken::new(), self.guard(), sink);
                return turn.finish(TurnEnd::with_error(StopReason::Error, e), started);
            }
        };
        let _running = Running(self.control.clone());
        // Spent between turns (a /compact) belongs to no turn.
        self.take_side_spend();
        let mut turn = Turn::new(cancel, self.guard(), sink);
        self.add_user(input, &mut turn).await;
        let end = self.steps(&mut turn).await;
        turn.finish(end, started)
    }

    /// A fresh guard per turn: its caps and clock start over.
    fn guard(&self) -> TurnGuard {
        TurnGuard::new(self.deps.hooks.clone(), self.deps.ledger.clone())
    }

    /// The hard caps for the current model spec; read fresh at every check.
    fn limits(&self) -> Option<TurnLimits> {
        self.deps.hooks.limits(&self.model_spec)
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

#[cfg(test)]
mod tests;
