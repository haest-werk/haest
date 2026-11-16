//! Neutral messages -> OpenAI Responses API params. Pure.
//!
//! OpenAI refuses function tools + reasoning effort together on Chat Completions
//! for its newer models, so the `openai` provider speaks Responses. We run it
//! stateless (`store: false`): every request carries the full history, and the
//! model's reasoning survives between turns only as `encrypted_content`, which we
//! keep in the thinking part's `signature` and replay to the SAME provider.

use kern_core::{
    messages::{AssistantMessage, AssistantPart, Message},
    provider::{Effort, ProviderRequest, ToolSpec},
};
use serde_json::{Map, Value, json};

use crate::{
    openai_media::{RESPONSES, with_tool_media},
    result_text,
};

/// gpt-5.5 accepts none|low|medium|high|xhigh ('max' and 'minimal' are 400s), so ours clamp onto those.
fn effort_name(effort: Effort) -> &'static str {
    match effort {
        Effort::Off => "none",
        Effort::Low => "low",
        Effort::Medium => "medium",
        Effort::High => "high",
        Effort::Xhigh | Effort::Max => "xhigh",
    }
}

/// None effort sends no `reasoning` at all: non-reasoning models reject the key.
pub fn reasoning_params(effort: Option<Effort>) -> Option<Value> {
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
        }
    }
}

/// A tool's media rides a user item after the run of function_call_outputs (same rule as Chat Completions).
pub fn to_responses_input(messages: &[Message], provider_id: &str) -> Vec<Value> {
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
    }
    });
    out
}

pub fn build_responses_body(provider_id: &str, req: &ProviderRequest) -> Value {
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
    Value::Object(body)
}
