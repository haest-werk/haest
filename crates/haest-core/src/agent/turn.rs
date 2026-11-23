//! The inside of one turn: steps, model calls, and the step boundary where
//! steering input and the guard's nudge ride in. The tools a step runs are
//! answered in delivery.rs; what the loop asks the host's hooks, in hooks.rs.

use std::time::Instant;

use tokio_util::sync::CancellationToken;

use super::tool_changes::{says_something, tool_change};
use crate::{
    agent::Agent,
    caps::estimate_request_tokens,
    deps::AgentHooks,
    error::report,
    events::{EventSink, LoopEvent, TurnOutcome},
    guard::TurnGuard,
    media::user_message,
    messages::{AssistantMessage, AssistantPart, Message, StopReason, ToolCall, Usage},
    provider::ProviderEvent,
    remedy::Limit,
    run::Journal,
    session::{interrupted, split_pending},
    tool_exec::{CANCELLED, error_assistant},
};

/// What a call of an answer is told when the turn ends before it ran, the
/// turn not cancelled (a hook's error).
const NOT_RUN: &str = "Not run: the turn ended before this tool call ran.";

/// One running turn's state.
pub(super) struct Turn<'s> {
    pub(super) cancel: CancellationToken,
    pub(super) usage: Usage,
    pub(super) guard: TurnGuard,
    pub(super) sink: &'s mut dyn EventSink,
    /// A durable run's records (durable.rs); None for a plain turn.
    pub(super) journal: Option<Journal>,
    /// The cap that refused the model call this step made, if one did: the
    /// turn's ending names it.
    refused_by: Option<Limit>,
}

impl<'s> Turn<'s> {
    pub(super) fn new(
        cancel: CancellationToken,
        guard: TurnGuard,
        sink: &'s mut dyn EventSink,
    ) -> Self {
        Self {
            cancel,
            usage: Usage::default(),
            guard,
            sink,
            journal: None,
            refused_by: None,
        }
    }

    /// The same turn as part of a durable run: its records go to `journal`
    /// too, and `usage` is what it spent before a resume.
    pub(super) fn journaled(mut self, journal: Journal, usage: Usage) -> Self {
        self.journal = Some(journal);
        self.usage = usage;
        self
    }

    /// What the turn has spent so far.
    pub(super) fn usage(&self) -> &Usage {
        &self.usage
    }

    /// A durable run's `turn` record: what the caps count now.
    fn record_turn(&self) {
        if let Some(j) = &self.journal {
            j.turn(self.guard.state(&self.usage));
        }
    }

    /// The `TurnEnd` event, then the same ending for the caller.
    pub(super) fn finish(&mut self, end: TurnOutcome, started: Instant) -> TurnOutcome {
        self.sink.emit(LoopEvent::TurnEnd {
            stop_reason: end.stop_reason,
            usage: self.usage.clone(),
            duration_ms: started.elapsed().as_millis() as u64,
            error: end.error.clone(),
        });
        end
    }
}

/// History grows here only: every message also goes to the persistence hook,
/// and in a durable run to its journal.
pub(super) fn append(
    messages: &mut Vec<Message>,
    hooks: &dyn AgentHooks,
    journal: Option<&Journal>,
    msg: Message,
) {
    hooks.on_message(&msg);
    if let Some(j) = journal {
        j.message(&msg);
    }
    messages.push(msg);
}

impl Agent {
    /// Every call in history with no result answered as interrupted, as a
    /// replayed log is: a turn dropped mid-tool, or a history handed in that
    /// ends there, left calls every provider refuses. The last answer's are
    /// appended like any message (the persistence hook records them); an
    /// earlier one, only in a history a host handed in, is answered in place.
    /// A result without its call, only in a history a host handed in, is
    /// dropped, as a replayed log's is.
    pub(crate) fn answer_dropped_calls(&mut self) {
        let repaired = split_pending(std::mem::take(&mut self.history));
        self.history = repaired.messages;
        for call in repaired.pending {
            append(
                &mut self.history,
                self.deps.shared().hooks().as_ref(),
                None,
                Message::Tool(interrupted(call)),
            );
        }
    }

    pub(super) async fn add_user(&mut self, text: &str, t: &mut Turn<'_>) {
        let user = user_message(text, self.deps.shared().hooks().as_ref(), &t.cancel).await;
        append(
            &mut self.history,
            self.deps.shared().hooks().as_ref(),
            t.journal.as_ref(),
            Message::User(user.clone()),
        );
        t.sink.emit(LoopEvent::User { message: user });
    }

    /// Queued steering input becomes a user message now; false when there was none.
    pub(super) async fn inject_queued(&mut self, t: &mut Turn<'_>) -> bool {
        let Some(text) = self.control.take_queued() else {
            return false;
        };
        self.add_user(&text, t).await;
        true
    }

    /// The guard's nudge goes to the model as input; the human sees it as a
    /// notice, not as their own message.
    fn inject_nudge(&mut self, text: Option<String>, t: &mut Turn<'_>) {
        let Some(text) = text else { return };
        append(
            &mut self.history,
            self.deps.shared().hooks().as_ref(),
            t.journal.as_ref(),
            Message::user(text.clone()),
        );
        t.sink.emit(LoopEvent::Guard { text });
    }

    /// The turn's thresholds at a step boundary: the reminders (the guard's
    /// nudge among them) reach the model as one input, warnings the human
    /// (through the host's hook), and the wind-down notice the model, once a
    /// turn.
    fn at_boundary(&mut self, t: &mut Turn<'_>) {
        let caps = self.turn_caps();
        let nudge = t.guard.nudge(&t.usage, caps.as_ref());
        self.inject_nudge(nudge, t);
        for text in t.guard.warnings(&t.usage, caps.as_ref()) {
            self.deps.shared().hooks().on_warning(&text);
        }
        let notice = t.guard.wind_down_notice(&t.usage, caps.as_ref());
        self.inject_nudge(notice, t);
    }

    /// A turn stops on its own only before a model call: every tool call already has its result.
    pub(super) async fn steps(&mut self, t: &mut Turn<'_>) -> TurnOutcome {
        loop {
            if t.cancel.is_cancelled() {
                return TurnOutcome::new(StopReason::Aborted);
            }
            if let Some(over) = t.guard.stop(&t.usage, self.turn_caps().as_ref()) {
                return over.into();
            }
            if let Some(end) = self.step(t).await {
                return end;
            }
        }
    }

    /// One model call and the tools it asked for. How the turn ends, or None to go on.
    async fn step(&mut self, t: &mut Turn<'_>) -> Option<TurnOutcome> {
        if let Some(end) = self.ask_before_model_call(t).await {
            return Some(end);
        }
        let msg = self.call_model(t).await;
        t.usage += &msg.usage;
        t.guard.called(&msg.usage);
        t.record_turn();
        if matches!(msg.stop_reason, StopReason::Error | StopReason::Aborted) {
            return Some(TurnOutcome {
                stop_reason: msg.stop_reason,
                error: msg.error_message,
                limit: t.refused_by.take(),
            });
        }
        self.answered(&msg, t).await
    }

    /// The rest of a step once `answer` landed in history: the host's look at
    /// it, then its tools, or the turn's end. How the turn ends, or None to go
    /// on. A resumed durable run (durable.rs) whose run stopped after an
    /// answer without tool calls goes on here.
    pub(super) async fn answered(
        &mut self,
        answer: &AssistantMessage,
        t: &mut Turn<'_>,
    ) -> Option<TurnOutcome> {
        let calls: Vec<ToolCall> = answer.tool_calls().cloned().collect();
        if let Some(end) = self.ask_after_model_call(answer, t).await {
            let why = if t.cancel.is_cancelled() {
                CANCELLED
            } else {
                NOT_RUN
            };
            self.answer_unrun(&calls, why, t);
            return Some(end);
        }
        if calls.is_empty() {
            return self
                .at_turn_end(TurnOutcome::new(answer.stop_reason), t)
                .await;
        }
        self.run_tools(&[], &calls, t).await;
        self.after_tools(t).await;
        None
    }

    /// The step boundary after a model call's tool results: the turn's
    /// thresholds ([`at_boundary`](Self::at_boundary)), then queued steering.
    /// A resumed durable run (durable.rs) ends its replayed step here too.
    pub(super) async fn after_tools(&mut self, t: &mut Turn<'_>) {
        self.at_boundary(t);
        self.inject_queued(t).await;
        t.record_turn();
    }

    async fn call_model(&mut self, t: &mut Turn<'_>) -> AssistantMessage {
        let resolved = match self.deps.shared().models().resolve(&self.model_spec) {
            Ok(r) => r,
            Err(e) => return refuse(&self.model_spec, &report(&e), t),
        };
        let prefix = self.request_prefix(resolved.model()).await;
        let mut messages = self.sendable(&self.history, resolved.model());
        // The one place a tool change is found: on the tools this call
        // would send, after every hook had its say.
        let change = tool_change(prefix.tool_baseline.as_deref(), &prefix.tools, &messages);
        messages.extend(change.clone().map(Message::ToolChange));
        let mut req = prefix.request(resolved.model().to_string(), messages, t.cancel.clone());
        // Sized before it is announced: a request too big to send never
        // went out, so no ModelStart says it did.
        let estimate = estimate_request_tokens(&req.system, &req.messages, req.sent_tools());
        if let Some(too_big) = t.guard.too_big(estimate, self.turn_caps().as_ref()) {
            t.refused_by = too_big.limit;
            return refuse(resolved.model(), &too_big.text, t);
        }
        t.sink.emit(LoopEvent::ModelStart {
            provider: resolved.provider().id().to_string(),
            model: resolved.model().to_string(),
        });
        let baseline = req.tool_baseline.clone();
        self.plan_cache(&mut req);
        self.cache.last_plan = Some(req.cache.clone());
        let sent_at = self.deps.shared().clock().now_ms();
        let mut msg = resolved
            .complete_streaming(req, |ev| match ev {
                ProviderEvent::TextDelta(text) => t.sink.emit(LoopEvent::TextDelta { text }),
                ProviderEvent::ThinkingDelta(text) => {
                    t.sink.emit(LoopEvent::ThinkingDelta { text })
                }
                ProviderEvent::ToolCallStart { id, name } => {
                    t.sink.emit(LoopEvent::ToolCallStart { id, name })
                }
                // The answer itself; complete_streaming keeps it.
                ProviderEvent::Done(_) => {}
            })
            .await;
        if matches!(msg.stop_reason, StopReason::Error | StopReason::Aborted) {
            // A failed answer runs no tool, so a call in it would stay in
            // history with no result, and the next request would be refused.
            msg.content
                .retain(|part| !matches!(part, AssistantPart::ToolCall(_)));
        } else {
            self.cache.answered(sent_at);
            self.cache.tool_baseline = baseline.map(|tools| (self.model_spec.clone(), tools));
            // Recorded with its answer, right before it: a call that failed
            // records nothing, and the next one sends the change again.
            if let Some(change) = change.filter(|_| says_something(&msg)) {
                append(
                    &mut self.history,
                    self.deps.shared().hooks().as_ref(),
                    t.journal.as_ref(),
                    Message::ToolChange(change),
                );
            }
        }
        append(
            &mut self.history,
            self.deps.shared().hooks().as_ref(),
            t.journal.as_ref(),
            Message::Assistant(msg.clone()),
        );
        t.sink.emit(LoopEvent::Assistant {
            message: msg.clone(),
        });
        msg
    }
}

/// A model call that never went out: shown, NOT appended -- nothing was sent,
/// and history must stay replayable.
fn refuse(model: &str, error: &str, t: &mut Turn<'_>) -> AssistantMessage {
    let msg = error_assistant(model, error);
    t.sink.emit(LoopEvent::Assistant {
        message: msg.clone(),
    });
    msg
}
