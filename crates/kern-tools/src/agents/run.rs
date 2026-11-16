//! Running ONE subagent's turn: what it is told (its note), what it may do (its
//! hooks: the conversation's, with its own caps and nothing persisted), and
//! what comes back (its last answer, or why there is none).

use std::{collections::HashSet, panic::AssertUnwindSafe, sync::Arc};

use async_trait::async_trait;
use futures::FutureExt;
use kern_core::{
    Agent, AgentHooks, LoopEvent, StopReason, SystemPrompt,
    catalog::{InputModality, Reasoning},
    limits::TurnLimits,
    messages::MediaPart,
    panics::panic_message,
    provider::Effort,
    tool::{InnerCallEvent, ToolHost, ToolSource},
};
use tokio_util::sync::CancellationToken;

use super::{AgentRef, SubagentCaps, SubagentEvent, SubagentHost};

const MINUTE_MS: u64 = 60_000;

/// One agent's run, as its driver needs it.
pub(super) struct Run {
    pub me: AgentRef,
    pub id: String,
    pub parent: String,
    pub description: String,
    pub stop: CancellationToken,
}

/// How one subagent's turn ended.
#[derive(Debug, Clone, PartialEq)]
pub struct RunOutcome {
    /// The final answer, or the reason there is none.
    pub text: String,
    pub cost_usd: f64,
    pub ok: bool,
}

/// Appended to a subagent's system prompt: who it is, who reads its answer, its caps, how to talk.
pub fn subagent_note(name: &str, parent: &str, description: &str, caps: &SubagentCaps) -> String {
    format!(
        "# You are a subagent

You are agent \"{name}\", started by \"{parent}\" for: {description}

Do the task, then end your turn with your COMPLETE answer: your final message is returned to \"{parent}\" verbatim, and nobody else reads your transcript. Nobody will answer questions -- if something is ambiguous, take the sensible reading and say which one you took.

Your caps: ${}, {} model calls, {} minutes. Messages from other agents arrive as user messages starting with [Message from agent ...]; reach a running agent (or \"main\", the conversation's own agent) with SendMessage.",
        caps.usd, caps.steps, caps.minutes
    )
}

/// The host's system prompt for this agent's model and tools, then its note.
pub(super) struct ChildPrompt {
    pub host: Arc<dyn SubagentHost>,
    pub model: String,
    pub tools: Arc<dyn ToolSource>,
    pub note: String,
}

#[async_trait]
impl SystemPrompt for ChildPrompt {
    async fn system_prompt(&self) -> String {
        let base = self
            .host
            .system_prompt(&self.model, &self.tools.tools())
            .await;
        format!("{base}\n\n{}", self.note)
    }
}

/// The conversation's hooks with the subagent's own turn caps. Its messages are
/// not the conversation's (nothing persisted, no compaction), and the calls its
/// tools make go to its own card.
pub(super) struct ChildHooks {
    pub parent: Arc<dyn AgentHooks>,
    pub caps: SubagentCaps,
    pub host: Arc<dyn SubagentHost>,
    pub id: String,
}

#[async_trait]
impl ToolHost for ChildHooks {
    async fn publish(&self, data: Vec<u8>, media_type: &str) -> Option<String> {
        self.parent.publish(data, media_type).await
    }
    fn on_inner_call(&self, ev: InnerCallEvent) {
        self.host
            .on_subagent(&self.id, SubagentEvent::InnerCall(ev));
    }
    fn log(&self, message: &str) {
        self.parent.log(message);
    }
}

#[async_trait]
impl AgentHooks for ChildHooks {
    fn limits(&self, spec: &str) -> Option<TurnLimits> {
        let base = self.parent.limits(spec).unwrap_or_default();
        Some(TurnLimits {
            max_turn_usd: self.caps.usd,
            max_turn_steps: self.caps.steps,
            max_turn_ms: self.caps.minutes * MINUTE_MS,
            ..base
        })
    }
    fn input_modalities(&self, spec: &str) -> Option<HashSet<InputModality>> {
        self.parent.input_modalities(spec)
    }
    async fn attach_media(
        &self,
        text: &str,
        cancel: &CancellationToken,
    ) -> (Vec<MediaPart>, Vec<String>) {
        self.parent.attach_media(text, cancel).await
    }
    fn on_media_withheld(&self, parts: &[MediaPart], model: &str) {
        self.parent.on_media_withheld(parts, model);
    }
    fn reasoning(&self, spec: &str) -> Option<Reasoning> {
        self.parent.reasoning(spec)
    }
    fn on_effort_fitted(&self, model: &str, used: Effort) {
        self.parent.on_effort_fitted(model, used);
    }
    fn now_ms(&self) -> u64 {
        self.parent.now_ms()
    }
}

fn reason(r: StopReason) -> String {
    serde_json::to_value(r)
        .ok()
        .and_then(|v| v.as_str().map(String::from))
        .unwrap_or_default()
}

fn outcome(stop: StopReason, error: Option<&str>, cost: Option<f64>, text: &str) -> RunOutcome {
    let ok = stop == StopReason::EndTurn && error.is_none();
    let why = if ok {
        String::new()
    } else {
        let why = error.map_or_else(|| reason(stop), String::from);
        format!("\n\n[the agent stopped early: {why}]")
    };
    let text = if text.is_empty() { "(no answer)" } else { text };
    RunOutcome {
        text: format!("{text}{why}"),
        cost_usd: cost.unwrap_or(0.0),
        ok,
    }
}

async fn cancelled(t: Option<&CancellationToken>) {
    match t {
        Some(t) => t.cancelled().await,
        None => std::future::pending().await,
    }
}

/// Run the turn to its end, every event to the host; never fails -- a crash
/// comes back as the agent's answer. `stop` (StopAgent) and `caller` (the
/// spawning turn, for a foreground agent) both abort it.
pub(super) async fn drive(
    agent: &mut Agent,
    prompt: &str,
    stop: &CancellationToken,
    caller: Option<&CancellationToken>,
    host: &dyn SubagentHost,
    id: &str,
) -> RunOutcome {
    let control = agent.control();
    let mut text = String::new();
    let mut ended = None;
    let result = {
        let mut sink = |ev: LoopEvent| {
            match &ev {
                LoopEvent::Assistant { message } => {
                    let said = message.text().trim().to_string();
                    if !said.is_empty() {
                        text = said;
                    }
                }
                LoopEvent::TurnEnd {
                    stop_reason,
                    usage,
                    error,
                    ..
                } => {
                    ended = Some(outcome(
                        *stop_reason,
                        error.as_deref(),
                        usage.cost_usd,
                        &text,
                    ))
                }
                _ => {}
            }
            host.on_subagent(id, SubagentEvent::Loop(ev));
        };
        let run = AssertUnwindSafe(agent.run(prompt, &mut sink)).catch_unwind();
        tokio::pin!(run);
        tokio::select! {
            biased;
            r = &mut run => r,
            _ = stop.cancelled() => { control.abort(); run.await }
            _ = cancelled(caller) => { control.abort(); run.await }
        }
    };
    match result {
        Ok(_) => ended.unwrap_or_else(|| outcome(StopReason::Error, None, None, "")),
        Err(p) => RunOutcome {
            text: format!(
                "[the agent crashed: {}]",
                panic_message(p.as_ref()).unwrap_or("a panic")
            ),
            cost_usd: 0.0,
            ok: false,
        },
    }
}
