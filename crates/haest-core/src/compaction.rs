//! Context compaction: when the last request filled most of the window,
//! summarise everything except the recent tail with the same model, and
//! continue on [summary, ...tail]. The summary request is the last loop request
//! plus one instruction (see [`SummaryPath`]), so it costs a cache read, not a re-read.
//!
//! The cut lands before a user or an assistant message, never between a tool
//! call and its result, so the tail never starts with an orphaned tool result
//! and every tool call in it keeps its result.

use std::collections::HashMap;

use futures::StreamExt;
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;

use crate::{
    agent::Agent,
    messages::{AssistantMessage, Message, StopReason, Usage},
    provider::{Provider, ProviderEvent, ProviderRequest},
    session::compaction_summary_message,
};

const SUMMARY_PROMPT: &str = "Summarize the conversation so far so work can continue without it. Include: the user's goals and constraints, decisions made, files read or changed (with paths), commands run and their outcomes, errors hit and how they were resolved, and exactly what remains to be done next. Be specific and complete; omit pleasantries. Reply with the summary only.";

const COLD_SYSTEM: &str = "You write precise working summaries of coding sessions.";

#[derive(Debug, Clone, PartialEq)]
pub struct CompactionConfig {
    pub context_window: u64,
    /// Fraction of the window that triggers compaction.
    pub threshold: f64,
    /// Absolute trigger in tokens when lower than the fraction (a 1M window at 80% is too late to be affordable).
    pub ceiling: Option<u64>,
    /// Roughly how many recent messages to keep verbatim.
    pub keep_recent: usize,
}

fn last_assistant(messages: &[Message]) -> Option<&AssistantMessage> {
    messages.iter().rev().find_map(|m| match m {
        Message::Assistant(a) if a.stop_reason != StopReason::Error => Some(a),
        _ => None,
    })
}

/// The LAST successful request filled the trigger (not the sum of every request).
pub fn needs_compaction(messages: &[Message], cfg: &CompactionConfig) -> bool {
    let fraction = cfg.context_window as f64 * cfg.threshold;
    let trigger = cfg.ceiling.map_or(fraction, |c| fraction.min(c as f64));
    last_assistant(messages).is_some_and(|a| a.usage.context_tokens() as f64 >= trigger)
}

/// Index of the first kept message: the latest at or before len-keep_recent
/// where a tail may start (see [`tail_starts`]); 0 = nothing to cut.
pub fn cut_index(messages: &[Message], keep_recent: usize) -> usize {
    if messages.is_empty() {
        return 0;
    }
    let start = (messages.len() - 1).min(messages.len().saturating_sub(keep_recent).max(1));
    let starts = tail_starts(messages);
    (1..=start).rev().find(|&i| starts[i]).unwrap_or(0)
}

/// Per message: may the kept tail start there? At a user message or an
/// assistant message (the summary is a user message, so roles still
/// alternate), never at a tool result, and never where a result after the cut
/// answers a call before it. One long agentic turn has no user message to cut
/// at, and must still compact.
fn tail_starts(messages: &[Message]) -> Vec<bool> {
    // For each result, where its call is: the latest call of that id before it.
    let mut called_at: HashMap<&str, usize> = HashMap::new();
    let mut call_of = vec![usize::MAX; messages.len()];
    for (i, m) in messages.iter().enumerate() {
        match m {
            Message::Assistant(a) => called_at.extend(a.tool_calls().map(|c| (c.id.as_str(), i))),
            Message::Tool(r) => {
                call_of[i] = called_at
                    .get(r.tool_call_id.as_str())
                    .copied()
                    .unwrap_or(usize::MAX)
            }
            Message::User(_) => {}
        }
    }
    let mut earliest_answered = usize::MAX;
    let mut starts = vec![false; messages.len()];
    for i in (0..messages.len()).rev() {
        earliest_answered = earliest_answered.min(call_of[i]);
        starts[i] = !matches!(messages[i], Message::Tool(_)) && earliest_answered >= i;
    }
    starts
}

fn warm_instruction(kept: usize) -> String {
    format!(
        "{SUMMARY_PROMPT}\n\nDo not call any tools: answer with text only. The last {kept} messages stay verbatim after your summary, so cover them briefly."
    )
}

/// warm: the main loop's last request again (same system, tools, effort, history) plus the summary
/// instruction, so the provider's prompt cache serves everything but the new rows.
/// cold: a standalone summary request; its prefix matches nothing, so the whole head is read at full price.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SummaryPath {
    Warm,
    Cold,
}

/// One summary call: what it cost, and its text if the reply is a summary, else why not.
struct Summary {
    text: Result<String, String>,
    usage: Usage,
}

/// The reply's text, if it is a whole summary: the call ended on its own,
/// called no tool, said something, and the run was not cancelled meanwhile.
/// Anything else (a stream that died, a reply cut off or cancelled part way,
/// "let me check X first" and a tool call) is a fragment, and committing it
/// would replace the head of history with it.
fn summary_text(msg: &AssistantMessage, cancel: &CancellationToken) -> Result<String, String> {
    let called_a_tool = msg.tool_calls().next().is_some();
    let why = match msg.stop_reason {
        _ if cancel.is_cancelled() => Some("cancelled"),
        StopReason::EndTurn if !called_a_tool => None,
        StopReason::EndTurn | StopReason::ToolUse => Some("the reply called a tool"),
        StopReason::MaxTokens => Some("the reply was cut off at its token limit"),
        StopReason::Refusal => Some("the model refused"),
        StopReason::Aborted => Some("cancelled"),
        StopReason::Error => Some(
            msg.error_message
                .as_deref()
                .unwrap_or("the provider failed"),
        ),
    };
    if let Some(why) = why {
        return Err(why.to_string());
    }
    let text = msg.text().trim().to_string();
    if text.is_empty() {
        return Err("empty response".into());
    }
    Ok(text)
}

async fn ask(provider: &dyn Provider, req: ProviderRequest) -> Summary {
    let cancel = req.cancel.clone();
    let mut done = None;
    let mut stream = provider.stream(req);
    while let Some(ev) = stream.next().await {
        if let ProviderEvent::Done(msg) = ev {
            done = Some(msg);
        }
    }
    match done {
        Some(msg) => Summary {
            text: summary_text(&msg, &cancel),
            usage: msg.usage,
        },
        None => Summary {
            text: Err("provider ended without a response".into()),
            usage: Usage::default(),
        },
    }
}

async fn summarize(
    agent: &Agent,
    head: &[Message],
    kept: usize,
    cancel: &CancellationToken,
) -> Result<(Summary, SummaryPath), CompactionError> {
    let resolved = agent
        .deps
        .models
        .resolve(&agent.model_spec)
        .map_err(CompactionError::Model)?;
    let (provider, model) = (resolved.provider.as_ref(), resolved.model);
    let prefix = agent.request_prefix(&model).await;
    let mut messages = agent.sendable(&agent.messages, &model);
    messages.push(Message::user(warm_instruction(kept)));
    let warm = ask(
        provider,
        prefix.request(model.clone(), messages, cancel.clone()),
    )
    .await;
    if warm.text.is_ok() || cancel.is_cancelled() {
        return Ok((warm, SummaryPath::Warm));
    }
    let mut messages = agent.sendable(head, &model);
    messages.push(Message::user(SUMMARY_PROMPT));
    let mut cold = ask(
        provider,
        ProviderRequest {
            model,
            system: COLD_SYSTEM.into(),
            messages,
            tools: Vec::new(),
            cancel: cancel.clone(),
            effort: None,
            max_tokens: None,
            web_search: None,
        },
    )
    .await;
    if let (Err(cold_why), Err(warm_why)) = (&mut cold.text, &warm.text) {
        *cold_why = format!("{cold_why} (the warm request: {warm_why})");
    }
    // The cold call came after the warm one: both were paid for.
    cold.usage.add(&warm.usage);
    Ok((cold, SummaryPath::Cold))
}

#[derive(Debug, Clone, PartialEq)]
pub struct CompactionResult {
    pub summary: String,
    /// Messages kept verbatim after the summary.
    pub kept: usize,
    pub path: SummaryPath,
    /// What every summary call cost, the warm one too when the cold one wrote
    /// it. Already counted toward the running turn's caps; the caller charges
    /// it to the conversation's ledger.
    pub usage: Usage,
}

#[derive(Debug, thiserror::Error)]
pub enum CompactionError {
    /// The agent's model spec resolves to no provider (no call was made).
    #[error("{0}")]
    Model(String),
    /// Neither path produced a summary, or the run was cancelled; history is
    /// untouched. Why, and what the calls made cost (see [`Self::usage`]).
    #[error("compaction summary failed: {0}")]
    Failed(String, Usage),
}

impl CompactionError {
    /// What the summary calls cost before it failed: a failure is paid for too.
    /// Already counted toward the running turn's caps; the caller charges it to
    /// the conversation's ledger.
    pub fn usage(&self) -> Usage {
        match self {
            CompactionError::Model(_) => Usage::default(),
            CompactionError::Failed(_, usage) => usage.clone(),
        }
    }
}

/// Compact in place. Returns the summary, how many messages were kept and how it
/// was written, or None if there is nothing to cut. On error history is untouched.
/// Every call it makes counts toward the running turn's caps, success or not.
pub async fn compact(
    agent: &mut Agent,
    cancel: &CancellationToken,
    cfg: &CompactionConfig,
) -> Result<Option<CompactionResult>, CompactionError> {
    let cut = cut_index(&agent.messages, cfg.keep_recent);
    if cut == 0 {
        return Ok(None);
    }
    let kept = agent.messages.len() - cut;
    let (res, path) = summarize(agent, &agent.messages[..cut], kept, cancel).await?;
    agent.spent_outside_the_loop(&res.usage);
    let summary = match res.text {
        Ok(summary) => summary,
        Err(why) => return Err(CompactionError::Failed(why, res.usage)),
    };
    agent
        .messages
        .splice(..cut, [compaction_summary_message(&summary)]);
    Ok(Some(CompactionResult {
        summary,
        kept,
        path,
        usage: res.usage,
    }))
}

#[cfg(test)]
mod tests;
