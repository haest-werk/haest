//! The inside of one turn: steps, model calls, tools, and the step boundary
//! where steering input and the guard's nudge ride in.

use std::{sync::Arc, time::Instant};

use tokio_util::sync::CancellationToken;

use crate::{
    agent::Agent,
    caps::{TurnCaps, estimate_request_tokens},
    deps::AgentHooks,
    events::{EventSink, LoopEvent, TurnEnd},
    guard::TurnGuard,
    media::user_message,
    messages::{AssistantMessage, AssistantPart, Message, StopReason, ToolCall, Usage},
    provider::{NO_RESPONSE, ProviderEvent, final_message},
    tool::{ToolContext, ToolServices},
    tool_batch::{Answer, Answers, run_tool_batches},
    tool_exec::{ToolIndex, error_assistant, tool_result_message},
};

/// One running turn's state.
pub(super) struct Turn<'s> {
    cancel: CancellationToken,
    usage: Usage,
    guard: TurnGuard,
    sink: &'s mut dyn EventSink,
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
        }
    }

    /// The `TurnEnd` event, then the same ending for the caller.
    pub(super) fn finish(&mut self, end: TurnEnd, started: Instant) -> TurnEnd {
        self.sink.emit(LoopEvent::TurnEnd {
            stop_reason: end.stop_reason,
            usage: self.usage.clone(),
            duration_ms: started.elapsed().as_millis() as u64,
            error: end.error.clone(),
        });
        end
    }
}

/// History grows here only: every message also goes to the persistence hook.
fn append(messages: &mut Vec<Message>, hooks: &dyn AgentHooks, msg: Message) {
    hooks.on_message(&msg);
    messages.push(msg);
}

impl Agent {
    pub(super) async fn add_user(&mut self, text: &str, t: &mut Turn<'_>) {
        let user = user_message(text, self.deps.hooks.as_ref(), &t.cancel).await;
        append(
            &mut self.history,
            self.deps.hooks.as_ref(),
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
            self.deps.hooks.as_ref(),
            Message::user(text.clone()),
        );
        t.sink.emit(LoopEvent::Guard { text });
    }

    /// A turn stops on its own only before a model call: every tool call already has its result.
    pub(super) async fn steps(&mut self, t: &mut Turn<'_>) -> TurnEnd {
        loop {
            if t.cancel.is_cancelled() {
                return TurnEnd::new(StopReason::Aborted);
            }
            if let Some(over) = t.guard.stop(&t.usage, self.turn_caps().as_ref()) {
                return TurnEnd::with_error(StopReason::MaxTokens, over);
            }
            if let Some(end) = self.step(t).await {
                return end;
            }
        }
    }

    /// One model call and the tools it asked for. How the turn ends, or None to go on.
    async fn step(&mut self, t: &mut Turn<'_>) -> Option<TurnEnd> {
        let cancel = t.cancel.clone();
        let ready = self
            .deps
            .hooks
            .clone()
            .before_model_call(self, &cancel)
            .await;
        // A summary the hook paid for counts toward this turn, and may stop it.
        let side = self.take_side_spend();
        t.usage.add(&side);
        // Cancelled while the hook ran (an Esc during the summary): the turn
        // was aborted, whatever the hook made of it.
        if cancel.is_cancelled() {
            return Some(TurnEnd::new(StopReason::Aborted));
        }
        if let Err(why) = ready {
            return Some(TurnEnd::with_error(StopReason::Error, why.to_string()));
        }
        if side != Usage::default()
            && let Some(over) = t.guard.stop(&t.usage, self.turn_caps().as_ref())
        {
            return Some(TurnEnd::with_error(StopReason::MaxTokens, over));
        }
        let msg = self.call_model(t).await;
        t.usage.add(&msg.usage);
        t.guard.called(&msg.usage);
        if matches!(msg.stop_reason, StopReason::Error | StopReason::Aborted) {
            return Some(TurnEnd {
                stop_reason: msg.stop_reason,
                error: msg.error_message,
            });
        }
        let calls: Vec<ToolCall> = msg.tool_calls().cloned().collect();
        if calls.is_empty() {
            let steered = self.inject_queued(t).await;
            return (!steered).then(|| TurnEnd::new(msg.stop_reason));
        }
        self.run_tools(&calls, t).await;
        let nudge = t.guard.nudge(&t.usage, self.turn_caps().as_ref());
        self.inject_nudge(nudge, t);
        self.inject_queued(t).await;
        None
    }

    async fn call_model(&mut self, t: &mut Turn<'_>) -> AssistantMessage {
        let resolved = match self.deps.models.resolve(&self.model_spec) {
            Ok(r) => r,
            Err(e) => return refuse(&self.model_spec, &e.to_string(), t),
        };
        t.sink.emit(LoopEvent::ModelStart {
            provider: resolved.provider.id().to_string(),
            model: resolved.model.clone(),
        });
        let prefix = self.request_prefix(&resolved.model).await;
        let messages = self.sendable(&self.history, &resolved.model);
        let estimate = estimate_request_tokens(&prefix.system, &messages, &prefix.tools);
        if let Some(too_big) = t.guard.too_big(estimate, self.turn_caps().as_ref()) {
            return refuse(&resolved.model, &too_big, t);
        }
        let req = prefix.request(resolved.model.clone(), messages, t.cancel.clone());
        let done = final_message(resolved.provider.stream(req), |ev| match ev {
            ProviderEvent::TextDelta(text) => t.sink.emit(LoopEvent::TextDelta { text }),
            ProviderEvent::ThinkingDelta(text) => t.sink.emit(LoopEvent::ThinkingDelta { text }),
            ProviderEvent::ToolCallStart { id, name } => {
                t.sink.emit(LoopEvent::ToolCallStart { id, name })
            }
            // final_message keeps it.
            ProviderEvent::Done(_) => {}
        })
        .await;
        let mut msg = done.unwrap_or_else(|| error_assistant(&resolved.model, NO_RESPONSE));
        if matches!(msg.stop_reason, StopReason::Error | StopReason::Aborted) {
            // A failed answer runs no tool, so a call in it would stay in
            // history with no result, and the next request would be refused.
            msg.content
                .retain(|part| !matches!(part, AssistantPart::ToolCall(_)));
        }
        append(
            &mut self.history,
            self.deps.hooks.as_ref(),
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
    async fn run_tools(&mut self, calls: &[ToolCall], t: &mut Turn<'_>) {
        let services: Arc<dyn ToolServices> = self.deps.hooks.clone();
        let ctx = ToolContext {
            cwd: self.deps.cwd.clone(),
            cancel: t.cancel.clone(),
            workspace: self.deps.workspace.clone(),
            services,
            context_room_tokens: None,
            tool_call_id: None,
            spill_dir: self.deps.spill_dir.clone(),
        };
        let tools = ToolIndex::new(self.deps.tools.callable());
        let mut delivery = Delivery {
            caps: self.turn_caps(),
            guard: &mut t.guard,
            messages: &mut self.history,
            hooks: self.deps.hooks.as_ref(),
            sink: &mut *t.sink,
        };
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

/// Where the loop's tool answers land: the guard, history, the sink.
struct Delivery<'a> {
    caps: Option<TurnCaps>,
    guard: &'a mut TurnGuard,
    messages: &'a mut Vec<Message>,
    hooks: &'a dyn AgentHooks,
    sink: &'a mut dyn EventSink,
}

impl Answers for Delivery<'_> {
    fn room(&self) -> Option<u64> {
        self.guard.room(self.caps.as_ref())
    }

    fn answer(&mut self, a: Answer) {
        self.guard.answered(&a.call, &a.output);
        let result = tool_result_message(&a.call, &a.output);
        append(self.messages, self.hooks, Message::Tool(result.clone()));
        self.sink.emit(LoopEvent::ToolResult {
            call: a.call,
            message: result,
            output: a.output,
            duration_ms: a.duration_ms,
        });
    }
}
