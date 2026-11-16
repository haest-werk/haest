//! Running ONE subagent's turn: what it is told (its note), what it may do (its
//! hooks: the conversation's, with its own caps and nothing persisted), and
//! what comes back (its last answer, or why there is none).

use std::{collections::HashSet, panic::AssertUnwindSafe, sync::Arc, time::Duration};

use async_trait::async_trait;
use futures::FutureExt;
use haest_core::{
    __private::panic_message,
    Agent, AgentHooks, LoopEvent, StopReason, SystemPrompt,
    caps::TurnCaps,
    catalog::{InputModality, ModelFacts, Reasoning},
    messages::MediaPart,
    provider::Effort,
    tool::{InnerCallEvent, ToolServices, ToolSource},
};
use tokio_util::sync::CancellationToken;

use super::{AgentRef, SubagentCaps, SubagentEvent, SubagentParent};

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
pub(super) struct RunOutcome {
    /// The final answer, or the reason there is none.
    pub text: String,
    /// What the turn spent, already charged to the ledger.
    pub cost_usd: f64,
    /// It ended its turn with an answer (no stop, no cap, no error).
    pub ok: bool,
}

/// The text appended to a subagent's system prompt: who it is (`name`), who
/// started it and reads its answer (`parent`), what for (`description`), its
/// caps, and how to talk to other agents.
pub fn subagent_note(name: &str, parent: &str, description: &str, caps: &SubagentCaps) -> String {
    format!(
        "# You are a subagent

You are agent \"{name}\", started by \"{parent}\" for: {description}

Do the task, then end your turn with your COMPLETE answer: your final message is returned to \"{parent}\" verbatim, and nobody else reads your transcript. Nobody will answer questions -- if something is ambiguous, take the sensible reading and say which one you took.

Your caps: ${}, {} model calls, {}. Messages from other agents arrive as user messages starting with [Message from agent ...]; reach a running agent (or \"main\", the conversation's own agent) with SendMessage.",
        caps.usd,
        caps.steps,
        wall_clock(caps.time)
    )
}

/// A subagent's time cap as its note says it: whole minutes, else seconds.
fn wall_clock(time: Duration) -> String {
    match time.as_secs() {
        s if s % 60 == 0 => format!("{} minutes", s / 60),
        s => format!("{s} seconds"),
    }
}

/// The parent's system prompt for this agent's model and tools, then its note.
pub(super) struct ChildPrompt {
    pub parent: Arc<dyn SubagentParent>,
    pub model_spec: String,
    pub tools: Arc<dyn ToolSource>,
    pub note: String,
}

#[async_trait]
impl SystemPrompt for ChildPrompt {
    async fn system_prompt(&self) -> String {
        let base = self
            .parent
            .system_prompt(&self.model_spec, &self.tools.tools())
            .await;
        format!("{base}\n\n{}", self.note)
    }
}

/// The conversation's hooks for a subagent. Its messages are not the
/// conversation's (nothing persisted, no compaction), and the calls its tools
/// make go to its own card.
pub(super) struct ChildHooks {
    pub hooks: Arc<dyn AgentHooks>,
    pub parent: Arc<dyn SubagentParent>,
    pub id: String,
}

/// The conversation's model facts with the subagent's own turn caps.
pub(super) struct ChildFacts {
    pub facts: Arc<dyn ModelFacts>,
    pub caps: SubagentCaps,
}

impl ModelFacts for ChildFacts {
    fn turn_caps(&self, spec: &str) -> Option<TurnCaps> {
        let base = self.facts.turn_caps(spec).unwrap_or_default();
        Some(TurnCaps {
            max_turn_usd: self.caps.usd,
            max_turn_steps: self.caps.steps,
            max_turn_time: self.caps.time,
            ..base
        })
    }
    fn input_modalities(&self, spec: &str) -> Option<HashSet<InputModality>> {
        self.facts.input_modalities(spec)
    }
    fn reasoning(&self, spec: &str) -> Option<Reasoning> {
        self.facts.reasoning(spec)
    }
}

#[async_trait]
impl ToolServices for ChildHooks {
    async fn publish(&self, data: Vec<u8>, media_type: &str) -> Option<String> {
        self.hooks.publish(data, media_type).await
    }
    fn on_inner_call(&self, ev: InnerCallEvent) {
        self.parent
            .on_subagent(&self.id, SubagentEvent::InnerCall(ev));
    }
    fn log(&self, message: &str) {
        self.hooks.log(message);
    }
}

#[async_trait]
impl AgentHooks for ChildHooks {
    async fn attach_media(
        &self,
        text: &str,
        cancel: &CancellationToken,
    ) -> (Vec<MediaPart>, Vec<String>) {
        self.hooks.attach_media(text, cancel).await
    }
    fn on_media_withheld(&self, parts: &[MediaPart], model: &str) {
        self.hooks.on_media_withheld(parts, model);
    }
    fn on_effort_fitted(&self, model: &str, used: Effort) {
        self.hooks.on_effort_fitted(model, used);
    }
}

fn outcome(stop: StopReason, error: Option<&str>, cost: Option<f64>, text: &str) -> RunOutcome {
    let ok = stop == StopReason::EndTurn && error.is_none();
    let why = if ok {
        String::new()
    } else {
        let why = error.unwrap_or(stop.as_str());
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

/// Run the turn to its end, every event to the parent; never fails -- a crash
/// comes back as the agent's answer. `stop` (StopAgent) and `caller` (the
/// spawning turn, for a foreground agent) both abort it.
pub(super) async fn drive(
    agent: &mut Agent,
    prompt: &str,
    stop: &CancellationToken,
    caller: Option<&CancellationToken>,
    parent: &dyn SubagentParent,
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
            parent.on_subagent(id, SubagentEvent::Loop(ev));
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
