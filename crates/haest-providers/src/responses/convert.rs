//! Neutral messages -> OpenAI Responses API params. Pure.
//!
//! OpenAI refuses function tools + reasoning effort together on Chat Completions
//! for its newer models, so the `openai` provider speaks Responses. We run it
//! stateless (`store: false`): every request carries the full history, and the
//! model's reasoning survives between turns only as `encrypted_content`, which we
//! keep in the thinking part's `signature` and replay to the SAME provider.

use haest_core::{
    messages::{AssistantMessage, AssistantPart, Message},
    provider::{Effort, ProviderRequest, ToolSpec},
};
use serde_json::{Map, Value, json};

use super::ResponsesOptions;
use crate::{
    openai_media::{RESPONSES, with_tool_media},
    result_text,
    send::is_openai_api,
};

/// gpt-5.5 accepts none|low|medium|high|xhigh ('max' and 'minimal' are 400s), so ours clamp onto those.
fn effort_name(effort: Effort) -> &'static str {
    match effort {
        Effort::Off => "none",
        Effort::Low => "low",
        Effort::Medium => "medium",
        Effort::High => "high",
        Effort::Xhigh | Effort::Max => "xhigh",
        // A level added later: high until it is named here.
        _ => "high",
    }
}

/// The `reasoning` field for `effort`: OpenAI's level (`off` is `none`,
/// `xhigh` and `max` are both `xhigh`) with an automatic summary.
/// None effort sends no `reasoning` at all: non-reasoning models reject the key.
pub(crate) fn reasoning_params(effort: Option<Effort>) -> Option<Value> {
    effort.map(|e| json!({"effort": effort_name(e), "summary": "auto"}))
}

fn to_responses_tools(tools: &[ToolSpec]) -> Vec<Value> {
    tools
        .iter()
        .map(|t| {
            json!({
                "type": "function",
                "name": t.name,
                "description": t.description,
                "parameters": t.parameters,
                "strict": false,
            })
        })
        .collect()
}

/// Parts keep their stream order: a reasoning item must precede the item it produced.
fn assistant_items(msg: &AssistantMessage, provider_id: &str, out: &mut Vec<Value>) {
    let replay = msg.provider == provider_id;
    for part in &msg.content {
        match part {
            AssistantPart::Thinking {
                thinking,
                signature,
                ..
            } => {
                let Some(sig) = signature.as_deref().filter(|s| replay && !s.is_empty()) else {
                    continue;
                };
                let summary = if thinking.is_empty() {
                    json!([])
                } else {
                    json!([{"type": "summary_text", "text": thinking}])
                };
                out.push(
                    json!({"type": "reasoning", "summary": summary, "encrypted_content": sig}),
                );
            }
            AssistantPart::Text { text } => {
                if !text.is_empty() {
                    out.push(json!({"role": "assistant", "content": text}));
                }
            }
            AssistantPart::ToolCall(c) => out.push(json!({
                "type": "function_call",
                "call_id": c.id,
                "name": c.name,
                "arguments": Value::Object(c.input.clone()).to_string(),
            })),
            // A kind of part the Responses API has no item for is not replayed.
            _ => {}
        }
    }
}

/// `messages` as the Responses API's `input` items, in order.
///
/// Reasoning replays, as its encrypted content, only from a message whose
/// provider is `provider_id`. A tool's media rides a user item after the run of
/// function_call_outputs (same rule as Chat Completions). A
/// [`Message::ToolChange`] is left out: the request carries the current tools.
pub(crate) fn to_responses_input(messages: &[Message], provider_id: &str) -> Vec<Value> {
    let mut out = Vec::new();
    with_tool_media(messages, &RESPONSES, &mut out, |m, out| {
        match m {
        Message::User(u) => out.push(
            json!({"role": "user", "content": RESPONSES.user_content(&u.content, &u.media)}),
        ),
        Message::Tool(t) => out.push(
            json!({"type": "function_call_output", "call_id": t.tool_call_id, "output": result_text(t)}),
        ),
        Message::Assistant(a) => assistant_items(a, provider_id, out),
        // The request carries the current tools instead.
        Message::ToolChange(_) => {}
    }
    });
    out
}

/// The streaming request body for `req`, as a [`ResponsesProvider`](super::ResponsesProvider)
/// with `opts` sends it.
///
/// `model`, the system prompt as `instructions`, the messages as `input`
/// ([`to_responses_input`]), the tools when there are any,
/// `max_output_tokens` when `req.max_tokens` is above 0, `reasoning` for the
/// effort ([`reasoning_params`]), `store: false` with the encrypted
/// reasoning included, so the next request can replay it, and the partition
/// key as `prompt_cache_key` when `opts` reach OpenAI's own API
/// ([`is_openai_api`]). The request's `cache_key` is never sent: here a key
/// partitions the cache, and a key per conversation would keep one
/// conversation from reading what another cached.
pub(crate) fn build_responses_body(opts: &ResponsesOptions, req: &ProviderRequest) -> Value {
    let provider_id = opts.id();
    let mut body = Map::new();
    body.insert("model".into(), json!(req.model));
    body.insert("instructions".into(), json!(req.system));
    body.insert(
        "input".into(),
        Value::Array(to_responses_input(&req.messages, provider_id)),
    );
    if !req.tools.is_empty() {
        body.insert("tools".into(), Value::Array(to_responses_tools(&req.tools)));
    }
    if let Some(max) = req.max_tokens.filter(|m| *m > 0) {
        body.insert("max_output_tokens".into(), json!(max));
    }
    if let Some(reasoning) = reasoning_params(req.effort) {
        body.insert("reasoning".into(), reasoning);
    }
    body.insert("store".into(), json!(false));
    body.insert("include".into(), json!(["reasoning.encrypted_content"]));
    body.insert("stream".into(), json!(true));
    if let Some(key) = req
        .cache
        .partition_key
        .as_deref()
        .filter(|_| is_openai_api(opts.base_url()))
    {
        body.insert("prompt_cache_key".into(), json!(key));
    }
    Value::Object(body)
}
