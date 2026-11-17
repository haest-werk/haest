//! Running ONE subagent's turn: what it is told (its note), what it may do (its
//! hooks: the conversation's, with its own caps and nothing persisted), and
//! what comes back (its last answer, or why there is none).

use std::{panic::AssertUnwindSafe, sync::Arc, time::Duration};

use async_trait::async_trait;
use futures::FutureExt;
use haest_core::{
    __private::panic_message,
    Agent, AgentHooks, AgentHooksRef, LoopEvent, StopReason, SystemPrompt,
    caps::{TurnCaps, TurnCapsSource, TurnCapsSourceRef},
    messages::MediaPart,
    provider::Effort,
    tool::{InnerCallEvent, ToolServices, ToolServicesRef, ToolSourceRef},
};
use tokio_util::sync::CancellationToken;

use super::{AgentRef, Ending, MemberCaps, Spend, SubagentEvent, SubagentParent};

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
    /// How it ended.
    pub ending: Ending,
    /// What the turn spent, already charged to the ledger.
    pub spend: Spend,
}

impl RunOutcome {
    /// It ended its turn with an answer (no stop, no cap, no error).
    pub fn ok(&self) -> bool {
        self.ending == Ending::Answered
    }
}

/// The text appended to a subagent's system prompt: who it is (`name`), who
/// started it and reads its answer (`parent`), what for (`description`), its
/// caps, and how to talk to other agents.
pub fn subagent_note(name: &str, parent: &str, description: &str, caps: &MemberCaps) -> String {
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

/// A wall clock as the model reads it: whole minutes, else seconds.
pub(super) fn wall_clock(time: Duration) -> String {
    match time.as_secs() {
        s if s % 60 == 0 => format!("{} minutes", s / 60),
        s => format!("{s} seconds"),
    }
}

/// The parent's system prompt for this agent's model and tools, then its note.
pub(super) struct ChildPrompt {
    pub parent: Arc<dyn SubagentParent>,
    pub model_spec: String,
    pub tools: ToolSourceRef,
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
/// conversation's (nothing persisted, no compaction).
pub(super) struct ChildHooks {
    pub hooks: AgentHooksRef,
}

/// The conversation's services for a subagent's tools: the calls they make
/// go to its own card.
pub(super) struct ChildServices {
    pub services: ToolServicesRef,
    pub parent: Arc<dyn SubagentParent>,
    pub id: String,
}

/// The conversation's turn caps, with a subagent's own spend, steps and time.
pub(super) struct ChildCaps {
    pub caps: TurnCapsSourceRef,
    pub subagent: MemberCaps,
}

impl TurnCapsSource for ChildCaps {
    fn turn_caps(&self, spec: &str) -> Option<TurnCaps> {
        let base = self.caps.turn_caps(spec).unwrap_or_default();
        Some(TurnCaps {
            max_turn_usd: self.subagent.usd,
            max_turn_steps: self.subagent.steps,
            max_turn_time: self.subagent.time,
            subagent: true,
            ..base
        })
    }
}

#[async_trait]
impl ToolServices for ChildServices {
    async fn publish(&self, data: Vec<u8>, media_type: &str) -> Option<String> {
        self.services.publish(data, media_type).await
    }
    fn on_inner_call(&self, ev: InnerCallEvent) {
        self.parent
            .on_subagent(&self.id, SubagentEvent::InnerCall(ev));
    }
    fn debug(&self, message: &str) {
        self.services.debug(message);
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

/// How the turn ended, from its `TurnEnd` (None: it sent none) and what
/// aborted it from outside (`aborted`).
fn ending(end: Option<&(StopReason, Option<String>)>, aborted: Option<Ending>) -> Ending {
    let Some((stop, error)) = end else {
        return aborted.unwrap_or_else(|| Ending::Failed(StopReason::Error.as_str().into()));
    };
    if *stop == StopReason::EndTurn && error.is_none() {
        return Ending::Answered;
    }
    if let Some(aborted) = aborted {
        return aborted;
    }
    let why = error.clone().unwrap_or_else(|| stop.as_str().into());
    match stop {
        StopReason::MaxTokens => Ending::Capped(why),
        _ => Ending::Failed(why),
    }
}

/// The answer as the caller reads it: the last text, and why the turn
/// stopped early when it did.
fn answer(text: &str, end: Option<&(StopReason, Option<String>)>, ok: bool) -> String {
    let text = if text.is_empty() { "(no answer)" } else { text };
    if ok {
        return text.to_string();
    }
    let why = match end {
        Some((_, Some(error))) => error.as_str(),
        Some((stop, None)) => stop.as_str(),
        None => StopReason::Error.as_str(),
    };
    format!("{text}\n\n[the agent stopped early: {why}]")
}

async fn cancelled(t: Option<&CancellationToken>) {
    match t {
        Some(t) => t.cancelled().await,
        None => std::future::pending().await,
    }
}

/// What stopped a member from outside, if anything did.
fn aborted(stop: &CancellationToken, caller: Option<&CancellationToken>) -> Option<Ending> {
    if stop.is_cancelled() {
        Some(Ending::Stopped)
    } else if caller.is_some_and(CancellationToken::is_cancelled) {
        Some(Ending::CallerCancelled)
    } else {
        None
    }
}

/// Run the turn to its end, every event to the parent; never fails -- a crash
/// comes back as the agent's answer. `stop` (StopAgent) and `caller` (the
/// spawning turn, for a foreground agent) both abort it; stopped before it
/// began (while its run started), it never runs.
pub(super) async fn drive(
    agent: &mut Agent,
    prompt: &str,
    stop: &CancellationToken,
    caller: Option<&CancellationToken>,
    parent: &dyn SubagentParent,
    id: &str,
) -> RunOutcome {
    if let Some(ending) = aborted(stop, caller) {
        let never = (StopReason::Aborted, None);
        return RunOutcome {
            text: answer("", Some(&never), false),
            ending,
            spend: Spend::default(),
        };
    }
    let control = agent.control();
    let mut text = String::new();
    let mut end = None;
    let mut spend = Spend::default();
    let result = {
        let mut sink = |ev: LoopEvent| {
            match &ev {
                LoopEvent::ModelStart { .. } => spend.model_calls += 1,
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
                    end = Some((*stop_reason, error.clone()));
                    spend.usage = usage.clone();
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
    // StopAgent aborts the loop directly too, so the run may end before
    // either token is seen above: ask the tokens, not the branch.
    let aborted = aborted(stop, caller);
    if let Err(p) = result {
        let why = panic_message(p.as_ref()).unwrap_or("a panic").to_string();
        return RunOutcome {
            text: format!("[the agent crashed: {why}]"),
            ending: Ending::Crashed(why),
            spend,
        };
    }
    let ending = ending(end.as_ref(), aborted);
    RunOutcome {
        text: answer(&text, end.as_ref(), ending == Ending::Answered),
        ending,
        spend,
    }
}
