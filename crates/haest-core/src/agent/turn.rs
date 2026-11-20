//! The inside of one turn: steps, model calls, tools, and the step boundary
//! where steering input and the guard's nudge ride in.

use std::{path::Path, time::Instant};

use tokio_util::sync::CancellationToken;

use crate::{
    agent::Agent,
    caps::{TurnCaps, estimate_request_tokens},
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
    tool::{Attempt, ToolContext},
    tool_batch::{Answer, Redo, redo_calls, run_tool_batches},
    tool_exec::{ToolIndex, error_assistant, tool_result_message},
};

/// One running turn's state.
pub(super) struct Turn<'s> {
    cancel: CancellationToken,
    usage: Usage,
    guard: TurnGuard,
    sink: &'s mut dyn EventSink,
    /// A durable run's records (durable.rs); None for a plain turn.
    journal: Option<Journal>,
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
fn append(
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
    async fn inject_queued(&mut self, t: &mut Turn<'_>) -> bool {
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
        let cancel = t.cancel.clone();
        let ready = self
            .deps
            .shared()
            .hooks()
            .clone()
            .before_model_call(self, &cancel)
            .await;
        // A summary the hook paid for counts toward this turn, and may stop it.
        let side = self.take_side_spend();
        t.usage += &side;
        // Cancelled while the hook ran (an Esc during the summary): the turn
        // was aborted, whatever the hook made of it.
        if cancel.is_cancelled() {
            return Some(TurnOutcome::new(StopReason::Aborted));
        }
        if let Err(why) = ready {
            return Some(TurnOutcome::error(StopReason::Error, why.to_string()));
        }
        if side != Usage::default()
            && let Some(over) = t.guard.stop(&t.usage, self.turn_caps().as_ref())
        {
            return Some(over.into());
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
        let calls: Vec<ToolCall> = msg.tool_calls().cloned().collect();
        if calls.is_empty() {
            let steered = self.inject_queued(t).await;
            return (!steered).then(|| TurnOutcome::new(msg.stop_reason));
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
        // Sized before it is announced: a request too big to send never
        // went out, so no ModelStart says it did.
        let prefix = self.request_prefix(resolved.model()).await;
        let messages = self.sendable(&self.history, resolved.model());
        let estimate = estimate_request_tokens(&prefix.system, &messages, &prefix.tools);
        if let Some(too_big) = t.guard.too_big(estimate, self.turn_caps().as_ref()) {
            t.refused_by = too_big.limit;
            return refuse(resolved.model(), &too_big.text, t);
        }
        t.sink.emit(LoopEvent::ModelStart {
            provider: resolved.provider().id().to_string(),
            model: resolved.model().to_string(),
        });
        let req = prefix.request(resolved.model().to_string(), messages, t.cancel.clone());
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

    /// Read-only calls run in parallel batches (tool_batch.rs); results still land
    /// in CALL order -- history, the guard's repeat count and the transcript all
    /// read them that way -- each one as soon as every call before it has answered.
    ///
    /// `redo` is a resumed durable run's (durable.rs): calls that may have run
    /// before the stop, answered one by one by their tool's `OnResume` before `calls`.
    pub(super) async fn run_tools(&mut self, redo: &[Redo], calls: &[ToolCall], t: &mut Turn<'_>) {
        let caps = self.turn_caps();
        let ctx = ToolContext {
            cwd: self.deps.shared().cwd().to_path_buf(),
            cancel: t.cancel.clone(),
            workspace: self.deps.shared().workspace().clone(),
            services: self.deps.shared().services().clone(),
            context_room_tokens: None,
            tool_call_id: None,
            spill_dir: self.deps.shared().spill_dir().map(Path::to_path_buf),
            run_key: t.journal.as_ref().map(|j| j.key().clone()),
            attempt: Attempt::First,
            turn_time_left: t.guard.time_left(caps.as_ref()),
            wind_down: t.guard.wind_down_watch(&t.usage, caps.clone()),
        };
        let tools = ToolIndex::new(self.deps.tools().callable());
        let mut delivery = Delivery {
            caps,
            guard: &mut t.guard,
            messages: &mut self.history,
            hooks: self.deps.shared().hooks().as_ref(),
            journal: t.journal.as_ref(),
            sink: &mut *t.sink,
        };
        redo_calls(redo, &tools, &ctx, &mut t.usage, &mut delivery).await;
        run_tool_batches(calls, &tools, &ctx, &mut t.usage, &mut delivery).await;
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

/// Where the loop's tool answers land: the guard, history, the sink, and a
/// durable run's journal.
pub(crate) struct Delivery<'a> {
    caps: Option<TurnCaps>,
    guard: &'a mut TurnGuard,
    messages: &'a mut Vec<Message>,
    hooks: &'a dyn AgentHooks,
    journal: Option<&'a Journal>,
    sink: &'a mut dyn EventSink,
}

impl Delivery<'_> {
    /// Tokens the next batch may add to the context; read once per batch, after
    /// every answer before it was delivered. None = uncapped.
    pub(crate) fn room(&self) -> Option<u64> {
        self.guard.room(self.caps.as_ref())
    }

    /// One answer, in CALL order; `usage` is the turn's, this answer's spend folded in.
    pub(crate) fn answer(&mut self, a: Answer, usage: &Usage) {
        self.guard.answered(&a.call, &a.output);
        let result = tool_result_message(&a.call, &a.output);
        append(
            self.messages,
            self.hooks,
            self.journal,
            Message::Tool(result.clone()),
        );
        if let Some(j) = self.journal {
            j.turn(self.guard.state(usage));
        }
        self.sink.emit(LoopEvent::ToolResult {
            call: a.call,
            message: result,
            output: a.output,
            duration_ms: a.duration_ms,
        });
    }

    /// Awaited before a batch that holds a call with side effects, so the
    /// intent lands first: a durable run's records, then the host's own flush.
    pub(crate) async fn before_effects(&mut self) {
        if let Some(j) = self.journal {
            j.flush().await;
        }
        self.hooks.before_effects().await;
    }
}
