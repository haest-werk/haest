//! The summary call: the loop's last request plus one instruction (warm),
//! and a standalone request over the head when that gives no summary (cold).

use tokio_util::sync::CancellationToken;

use super::{CompactionError, SummaryPath};
use crate::{
    agent::Agent,
    messages::{AssistantMessage, Message, StopReason, Usage},
    provider::{NO_RESPONSE, Provider, ProviderRequest, final_message},
};

const SUMMARY_PROMPT: &str = "Summarize the conversation so far so work can continue without it. Include: the user's goals and constraints, decisions made, files read or changed (with paths), commands run and their outcomes, errors hit and how they were resolved, and exactly what remains to be done next. Be specific and complete; omit pleasantries. Reply with the summary only.";

const COLD_SYSTEM: &str = "You write precise working summaries of coding sessions.";

fn warm_instruction(kept: usize) -> String {
    format!(
        "{SUMMARY_PROMPT}\n\nDo not call any tools: answer with text only. The last {kept} messages stay verbatim after your summary, so cover them briefly."
    )
}

/// One summary call: what it cost, and its text if the reply is a summary, else why not.
pub(super) struct Summary {
    pub(super) text: Result<String, String>,
    pub(super) usage: Usage,
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
    match final_message(provider.stream(req), |_| {}).await {
        Some(msg) => Summary {
            text: summary_text(&msg, &cancel),
            usage: msg.usage,
        },
        None => Summary {
            text: Err(NO_RESPONSE.into()),
            usage: Usage::default(),
        },
    }
}

pub(super) async fn summarize(
    agent: &Agent,
    head: &[Message],
    kept: usize,
    cancel: &CancellationToken,
) -> Result<(Summary, SummaryPath), CompactionError> {
    let resolved = agent
        .deps()
        .models
        .resolve(agent.model_spec())
        .map_err(CompactionError::Model)?;
    let (provider, model) = (resolved.provider.as_ref(), resolved.model);
    let prefix = agent.request_prefix(&model).await;
    let mut messages = agent.sendable(agent.history(), &model);
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
