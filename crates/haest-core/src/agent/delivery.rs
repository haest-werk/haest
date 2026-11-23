//! Where a turn's tool calls are answered: run in batches ([`Agent::run_tools`]),
//! or answered without running when the turn ends before they ran
//! ([`Agent::answer_unrun`]). Every answer lands through one [`Delivery`].

use std::path::Path;

use crate::{
    agent::{
        Agent,
        turn::{Turn, append},
    },
    caps::TurnCaps,
    deps::AgentHooks,
    events::{EventSink, LoopEvent},
    guard::TurnGuard,
    messages::{Message, ToolCall, Usage},
    run::Journal,
    tool::{Attempt, ToolContext, ToolOutput},
    tool_batch::{Answer, Redo, redo_calls, run_tool_batches},
    tool_exec::{ToolIndex, tool_result_message},
};

impl Agent {
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
        let (mut delivery, usage) = self.delivery(caps, t);
        redo_calls(redo, &tools, &ctx, usage, &mut delivery).await;
        run_tool_batches(calls, &tools, &ctx, usage, &mut delivery).await;
    }

    /// Each of `calls` answered with the error `why`, none run: the turn ends
    /// before they ran, and every call keeps its result so the history can be
    /// sent again.
    pub(super) fn answer_unrun(&mut self, calls: &[ToolCall], why: &str, t: &mut Turn<'_>) {
        let caps = self.turn_caps();
        let (mut delivery, usage) = self.delivery(caps, t);
        for call in calls {
            let answer = Answer {
                call: call.clone(),
                output: ToolOutput::error(why),
                duration_ms: 0,
            };
            delivery.answer(answer, usage);
        }
    }

    /// Where this turn's tool answers land, and the turn's usage beside it.
    fn delivery<'a>(
        &'a mut self,
        caps: Option<TurnCaps>,
        t: &'a mut Turn<'_>,
    ) -> (Delivery<'a>, &'a mut Usage) {
        let Turn {
            usage,
            guard,
            sink,
            journal,
            ..
        } = t;
        let delivery = Delivery {
            caps,
            guard,
            messages: &mut self.history,
            hooks: self.deps.shared().hooks().as_ref(),
            journal: journal.as_ref(),
            sink: &mut **sink,
        };
        (delivery, usage)
    }
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
