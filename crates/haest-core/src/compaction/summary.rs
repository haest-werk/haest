//! The summary call: the loop's last request plus one instruction (warm),
//! and a standalone request over the head when that gives no summary (cold).

use tokio_util::sync::CancellationToken;

use super::{CompactionError, SummaryPath};
use crate::{
    agent::Agent,
    messages::{AssistantMessage, Message, StopReason, Usage},
    provider::{ProviderRequest, ResolvedModel},
};

const SUMMARY_PROMPT: &str = "Summarize the conversation so far so work can continue without it. Include: the user's goals and constraints, decisions made, files read or changed (with paths), commands run and their outcomes, errors hit and how they were resolved, and exactly what remains to be done next. Be specific and complete; omit pleasantries. Reply with the summary only.";

const COLD_SYSTEM: &str = "You write precise working summaries of coding sessions.";

/// haest's summary instruction, then the caller's own when it has some.
fn summary_prompt(asked: Option<&str>) -> String {
    match asked {
        Some(asked) => format!("{SUMMARY_PROMPT}\n\nFor this summary, also: {asked}"),
        None => SUMMARY_PROMPT.to_string(),
    }
}

fn warm_instruction(kept: usize, asked: Option<&str>) -> String {
    let prompt = summary_prompt(asked);
    format!(
        "{prompt}\n\nDo not call any tools: answer with text only. The last {kept} messages stay verbatim after your summary, so cover them briefly."
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

async fn ask(model: &ResolvedModel, req: ProviderRequest) -> Summary {
    let cancel = req.cancel.clone();
    let msg = model.complete(req).await;
    Summary {
        text: summary_text(&msg, &cancel),
        usage: msg.usage,
    }
}

/// The summary of the first `cut` messages, `kept` staying after it, with
/// the caller's `asked` instructions: warm, then cold when the warm reply is
/// no summary. Each call is booked ([`Agent::spent_outside_the_loop`]) as it
/// comes back, so a compaction dropped before it ends has booked every call
/// that came back.
pub(super) async fn summarize(
    agent: &mut Agent,
    cut: usize,
    kept: usize,
    asked: Option<&str>,
    cancel: &CancellationToken,
) -> Result<(Summary, SummaryPath), CompactionError> {
    let resolved = agent
        .deps()
        .shared()
        .models()
        .resolve(agent.model_spec())
        .map_err(CompactionError::Model)?;
    let model = resolved.model().to_string();
    let prefix = agent.request_prefix(&model).await;
    let mut messages = agent.sendable(agent.history(), &model);
    messages.push(Message::user(warm_instruction(kept, asked)));
    let mut req = prefix.request(model.clone(), messages, cancel.clone());
    agent.plan_cache(&mut req);
    let warm = ask(&resolved, req).await;
    agent.spent_outside_the_loop(&warm.usage);
    if warm.text.is_ok() || cancel.is_cancelled() {
        return Ok((warm, SummaryPath::Warm));
    }
    let mut messages = agent.sendable(&agent.history()[..cut], &model);
    messages.push(Message::user(summary_prompt(asked)));
    let req = ProviderRequest::new(model, messages)
        .with_system(COLD_SYSTEM)
        .with_cancel(cancel.clone());
    let mut cold = ask(&resolved, req).await;
    agent.spent_outside_the_loop(&cold.usage);
    if let (Err(cold_why), Err(warm_why)) = (&mut cold.text, &warm.text) {
        *cold_why = format!("{cold_why} (the warm request: {warm_why})");
    }
    // The cold call came after the warm one: both were paid for.
    cold.usage += &warm.usage;
    Ok((cold, SummaryPath::Cold))
}
