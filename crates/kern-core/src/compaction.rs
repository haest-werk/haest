//! Context compaction: when the last request filled most of the window,
//! summarise everything except the recent tail with the same model, and
//! continue on [summary, ...tail]. The summary request is the last loop request
//! plus one instruction (see [`SummaryPath`]), so it costs a cache read, not a re-read.
//!
//! The cut always lands on a USER message, so the tail never starts with an
//! orphaned tool result and every tool call in it keeps its result.

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

/// Index of the first kept message: the latest user message at or before len-keep_recent.
pub fn cut_index(messages: &[Message], keep_recent: usize) -> usize {
    if messages.is_empty() {
        return 0;
    }
    let start = (messages.len() - 1).min(messages.len().saturating_sub(keep_recent).max(1));
    (1..=start)
        .rev()
        .find(|&i| matches!(messages[i], Message::User(_)))
        .unwrap_or(0)
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

struct Summary {
    text: String,
    usage: Usage,
    error: Option<String>,
}

async fn ask(provider: &dyn Provider, req: ProviderRequest) -> Summary {
    let mut out = Summary {
        text: String::new(),
        usage: Usage::default(),
        error: Some("provider ended without a response".into()),
    };
    let mut stream = provider.stream(req);
    while let Some(ev) = stream.next().await {
        if let ProviderEvent::Done(msg) = ev {
            out = Summary {
                text: msg.text().trim().to_string(),
                usage: msg.usage,
                error: msg.error_message,
            };
        }
    }
    out
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
    if !warm.text.is_empty() || cancel.is_cancelled() {
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
    cold.error = cold.error.or(warm.error);
    Ok((cold, SummaryPath::Cold))
}

#[derive(Debug, Clone, PartialEq)]
pub struct CompactionResult {
    pub summary: String,
    /// Messages kept verbatim after the summary.
    pub kept: usize,
    pub path: SummaryPath,
    /// What the summary call cost (the caller charges it).
    pub usage: Usage,
}

#[derive(Debug, thiserror::Error)]
pub enum CompactionError {
    /// The agent's model spec resolves to no provider.
    #[error("{0}")]
    Model(String),
    /// Neither path produced a summary; history is untouched.
    #[error("compaction summary failed: {0}")]
    Failed(String),
}

/// Compact in place. Returns the summary, how many messages were kept and how it
/// was written, or None if there is nothing to cut. On error history is untouched.
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
    if res.text.is_empty() {
        let why = res.error.unwrap_or_else(|| "empty response".into());
        return Err(CompactionError::Failed(why));
    }
    agent
        .messages
        .splice(..cut, [compaction_summary_message(&res.text)]);
    Ok(Some(CompactionResult {
        summary: res.text,
        kept,
        path,
        usage: res.usage,
    }))
}

#[cfg(test)]
mod tests;
