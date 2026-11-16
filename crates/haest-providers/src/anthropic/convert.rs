//! Neutral messages -> Anthropic Messages API params. Pure.
//!
//! Rules the API enforces that shape this file:
//!   - roles strictly alternate, so consecutive same-role messages merge
//!     (tool results + a queued user message become ONE user turn);
//!   - every tool_use needs its tool_result in the very next user turn;
//!   - thinking blocks replay only with their signature, and only to Anthropic.

use haest_core::{
    media::dropped_note,
    messages::{AssistantMessage, AssistantPart, MediaKind, MediaPart, Message, ToolResultMessage},
    provider::{Effort, ToolSpec},
};
use serde_json::{Map, Value, json};

use crate::result_text;

fn cache() -> Value {
    json!({"type": "ephemeral"})
}

fn safe_tool_id(id: &str) -> String {
    let safe: String = id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    if safe.is_empty() { "tool".into() } else { safe }
}

fn assistant_blocks(msg: &AssistantMessage) -> Vec<Value> {
    let replay_thinking = msg.provider == "anthropic";
    let mut out = Vec::new();
    for part in &msg.content {
        match part {
            AssistantPart::Thinking {
                thinking,
                signature,
                redacted,
            } => {
                if !replay_thinking {
                    continue;
                }
                if let Some(data) = redacted.as_deref().filter(|d| !d.is_empty()) {
                    out.push(json!({"type": "redacted_thinking", "data": data}));
                } else if let Some(sig) = signature.as_deref().filter(|s| !s.is_empty()) {
                    out.push(json!({"type": "thinking", "thinking": thinking, "signature": sig}));
                }
            }
            AssistantPart::Text { text } => {
                if !text.is_empty() {
                    out.push(json!({"type": "text", "text": text}));
                }
            }
            AssistantPart::ToolCall(c) => out.push(json!({
                "type": "tool_use",
                "id": safe_tool_id(&c.id),
                "name": c.name,
                "input": c.input,
            })),
        }
    }
    out
}

/// Images and PDFs go as base64 blocks; Anthropic takes no audio or video, so those become a note.
fn media_block(p: &MediaPart) -> Value {
    match p.kind {
        MediaKind::Image => json!({
            "type": "image",
            "source": {"type": "base64", "media_type": p.media_type, "data": p.data},
        }),
        MediaKind::File => json!({
            "type": "document",
            "title": p.name,
            "source": {"type": "base64", "media_type": "application/pdf", "data": p.data},
        }),
        MediaKind::Audio | MediaKind::Video => {
            json!({"type": "text", "text": dropped_note(p, "Anthropic")})
        }
    }
}

fn user_blocks(text: &str, media: &[MediaPart]) -> Vec<Value> {
    let lead = (!text.is_empty()).then(|| json!({"type": "text", "text": text}));
    lead.into_iter()
        .chain(media.iter().map(media_block))
        .collect()
}

fn tool_result_block(msg: &ToolResultMessage) -> Value {
    let text = result_text(msg);
    let content = if msg.media.is_empty() {
        Value::String(text.to_string())
    } else {
        Value::Array(user_blocks(text, &msg.media))
    };
    let mut block = json!({
        "type": "tool_result",
        "tool_use_id": safe_tool_id(&msg.tool_call_id),
        "content": content,
    });
    if msg.is_error {
        block["is_error"] = Value::Bool(true);
    }
    block
}

fn blocks_of(msg: &Message) -> (&'static str, Vec<Value>) {
    match msg {
        Message::User(u) => ("user", user_blocks(&u.content, &u.media)),
        Message::Tool(t) => ("user", vec![tool_result_block(t)]),
        Message::Assistant(a) => ("assistant", assistant_blocks(a)),
    }
}

pub fn to_anthropic_messages(messages: &[Message]) -> Vec<Value> {
    let mut out: Vec<(&'static str, Vec<Value>)> = Vec::new();
    for msg in messages {
        let (role, blocks) = blocks_of(msg);
        if blocks.is_empty() {
            continue;
        }
        match out.last_mut() {
            Some((last, content)) if *last == role => content.extend(blocks),
            _ => out.push((role, blocks)),
        }
    }
    // Rolling cache breakpoint on the newest block: the next request re-reads
    // everything up to here from cache.
    if let Some(tail) = out.last_mut().and_then(|(_, c)| c.last_mut()) {
        let kind = tail.get("type").and_then(Value::as_str);
        if kind != Some("thinking") && kind != Some("redacted_thinking") {
            tail["cache_control"] = cache();
        }
    }
    out.into_iter()
        .map(|(role, content)| json!({"role": role, "content": content}))
        .collect()
}

pub fn to_anthropic_tools(tools: &[ToolSpec]) -> Vec<Value> {
    tools
        .iter()
        .enumerate()
        .map(|(i, t)| {
            let mut tool = json!({
                "name": t.name,
                "description": t.description,
                "input_schema": t.parameters,
            });
            if i + 1 == tools.len() {
                tool["cache_control"] = cache();
            }
            tool
        })
        .collect()
}

/// `model` contains `prefix` followed by one of `digits` and then a word boundary.
fn has_version(model: &str, prefix: &str, digits: &[u8]) -> bool {
    model.match_indices(prefix).any(|(i, _)| {
        let rest = &model.as_bytes()[i + prefix.len()..];
        match rest {
            [d, tail @ ..] if digits.contains(d) => tail
                .first()
                .is_none_or(|c| !(c.is_ascii_alphanumeric() || *c == b'_')),
            _ => false,
        }
    })
}

/// Models that predate adaptive thinking take a token budget instead (4.5 and older).
pub fn uses_budget_thinking(model: &str) -> bool {
    model.contains("claude-3")
        || ["opus", "sonnet", "haiku"]
            .iter()
            .any(|f| has_version(model, &format!("claude-{f}-4-"), b"015"))
        || ["opus", "sonnet"]
            .iter()
            .any(|f| model.contains(&format!("claude-{f}-4-2025")))
}

fn budget(effort: Effort) -> u32 {
    match effort {
        Effort::Off | Effort::Low => 2048,
        Effort::Medium => 8192,
        Effort::High => 16384,
        Effort::Xhigh => 24576,
        Effort::Max => 31999,
    }
}

/// `thinking` (+ `output_config`) for `effort`; empty for none or `off`.
pub fn thinking_params(model: &str, effort: Option<Effort>) -> Map<String, Value> {
    let mut out = Map::new();
    let Some(effort) = effort.filter(|e| *e != Effort::Off) else {
        return out;
    };
    if uses_budget_thinking(model) {
        out.insert(
            "thinking".into(),
            json!({"type": "enabled", "budget_tokens": budget(effort)}),
        );
    } else {
        out.insert("thinking".into(), json!({"type": "adaptive"}));
        out.insert("output_config".into(), json!({"effort": effort}));
    }
    out
}
