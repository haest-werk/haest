//! Neutral messages -> Anthropic Messages API params. Pure.
//!
//! Rules the API enforces that shape this file:
//!   - roles strictly alternate, so consecutive same-role messages merge
//!     (tool results + a queued user message become ONE user turn);
//!   - every tool_use needs its tool_result in the very next user turn;
//!   - thinking blocks replay only with their signature, and only to the
//!     provider (the endpoint) that signed them;
//!   - a mid-conversation `system` message (a tool change) follows a user
//!     turn and precedes an assistant turn or ends the array; the loop
//!     places it so, this file only renders it.

use haest_core::{
    cache::{CachePlan, CacheTtl},
    media::dropped_note,
    messages::{
        AssistantMessage, AssistantPart, MediaKind, MediaPart, Message, ToolChange,
        ToolResultMessage,
    },
    provider::{Effort, ProviderRequest, ToolSpec},
};
use serde_json::{Map, Value, json};

use crate::{cache::cache_control, result_text};

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

fn assistant_blocks(msg: &AssistantMessage, provider_id: &str) -> Vec<Value> {
    let replay_thinking = msg.provider == provider_id;
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
            // A kind of part Anthropic has no block for is not replayed.
            _ => {}
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
        // Audio, video, and any kind added later.
        _ => json!({"type": "text", "text": dropped_note(p, "Anthropic")}),
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

/// The `tool_addition` / `tool_removal` blocks of a mid-conversation system
/// message, as Anthropic documents them: a removal and a tool of the
/// baseline by reference, any other tool by its definition, a `tools` entry
/// as [`to_anthropic_tools`] writes one, without a breakpoint.
fn tool_change_blocks(changes: &[ToolChange]) -> Vec<Value> {
    let reference = |name: &str| json!({"type": "tool_reference", "name": name});
    changes
        .iter()
        .filter_map(|change| match change {
            ToolChange::Removed { name } => {
                Some(json!({"type": "tool_removal", "tool": reference(name)}))
            }
            ToolChange::Restored { name } => {
                Some(json!({"type": "tool_addition", "tool": reference(name)}))
            }
            ToolChange::Added { tool } => {
                let definition = to_anthropic_tools(std::slice::from_ref(tool), None).pop()?;
                Some(json!({"type": "tool_addition",
                    "tool": {"type": "tool_definition", "definition": definition}}))
            }
            // A kind of change Anthropic has no block for is not sent.
            _ => None,
        })
        .collect()
}

/// Which role `msg` takes in the Messages API, and its blocks. A tool change
/// is a `system` message when `tool_changes`, else nothing.
fn blocks_of(msg: &Message, provider_id: &str, tool_changes: bool) -> (&'static str, Vec<Value>) {
    match msg {
        Message::User(u) => ("user", user_blocks(&u.content, &u.media)),
        Message::Tool(t) => ("user", vec![tool_result_block(t)]),
        Message::Assistant(a) => ("assistant", assistant_blocks(a, provider_id)),
        Message::ToolChange(c) if tool_changes => (SYSTEM, tool_change_blocks(&c.changes)),
        Message::ToolChange(_) => (SYSTEM, Vec::new()),
    }
}

/// The role of a mid-conversation system message: the tool changes.
const SYSTEM: &str = "system";

/// Whether `req` sends a tool change: it has a tool baseline and a
/// [`Message::ToolChange`] with a change in it. Such a request needs the
/// beta header [`INLINE_TOOLS_BETA`](super::INLINE_TOOLS_BETA).
pub(crate) fn sends_tool_changes(req: &ProviderRequest) -> bool {
    req.tool_baseline.is_some()
        && req.messages.iter().any(
            |m| matches!(m, Message::ToolChange(c) if !tool_change_blocks(&c.changes).is_empty()),
        )
}

/// A thinking block, which takes no cache breakpoint.
fn is_thinking(block: &Value) -> bool {
    let kind = block.get("type").and_then(Value::as_str);
    kind == Some("thinking") || kind == Some("redacted_thinking")
}

/// `messages` as the Messages API's `messages` array, with the breakpoints
/// `cache` places on them (a [`CacheTtl`] is the standard plan at that TTL).
///
/// Consecutive messages of one role merge into one turn (tool results and a
/// queued user message become one user turn). Thinking replays only from a
/// message whose provider is `provider_id`, and only with its signature. Images
/// and PDFs go as base64 blocks; audio and video, which the API does not take,
/// become a text note. With `tool_changes`, a [`Message::ToolChange`] is a
/// `system` message of `tool_addition` / `tool_removal` blocks; without, it
/// is left out. The newest block outside a system message gets the rolling
/// breakpoint, with `cache.history`'s TTL, as Anthropic advises for a new
/// system message (the next request's breakpoint moves past it); a message
/// the plan chose gets one on its last block that is not thinking, with its
/// own, and a chosen tool change none.
pub(crate) fn to_anthropic_messages(
    messages: &[Message],
    provider_id: &str,
    cache: impl Into<CachePlan>,
    tool_changes: bool,
) -> Vec<Value> {
    let cache = cache.into();
    let mut out: Vec<(&'static str, Vec<Value>)> = Vec::new();
    // Each chosen message's blocks: its TTL, its turn, where its blocks start.
    let mut chosen = Vec::new();
    for (i, msg) in messages.iter().enumerate() {
        let (role, blocks) = blocks_of(msg, provider_id, tool_changes);
        if blocks.is_empty() {
            continue;
        }
        let count = blocks.len();
        match out.last_mut() {
            Some((last, content)) if *last == role => content.extend(blocks),
            _ => out.push((role, blocks)),
        }
        let turn = out.len() - 1;
        let start = out[turn].1.len() - count;
        let ttl = cache.messages.iter().find(|(at, _)| *at == i).map(|m| m.1);
        chosen.extend(ttl.filter(|_| role != SYSTEM).map(|ttl| (ttl, turn, start)));
    }
    for (ttl, turn, start) in chosen {
        let last = out[turn].1[start..]
            .iter_mut()
            .rev()
            .find(|b| !is_thinking(b));
        if let Some(block) = last {
            block["cache_control"] = cache_control(ttl);
        }
    }
    // Rolling cache breakpoint on the newest block: the next request re-reads
    // everything up to here from cache. A system message never takes one.
    let tail = (out.iter_mut().rev())
        .find(|(role, _)| *role != SYSTEM)
        .and_then(|(_, c)| c.last_mut());
    if let (Some(tail), Some(ttl)) = (tail, cache.history)
        && !is_thinking(tail)
    {
        tail["cache_control"] = cache_control(ttl);
    }
    out.into_iter()
        .map(|(role, content)| json!({"role": role, "content": content}))
        .collect()
}

/// `tools` as Messages API tool definitions, the last with a prompt-cache
/// breakpoint that lives `ttl` (None: no breakpoint).
pub(crate) fn to_anthropic_tools(
    tools: &[ToolSpec],
    ttl: impl Into<Option<CacheTtl>>,
) -> Vec<Value> {
    let ttl = ttl.into();
    tools
        .iter()
        .enumerate()
        .map(|(i, t)| {
            let mut tool = json!({
                "name": t.name,
                "description": t.description,
                "input_schema": t.parameters,
            });
            if let Some(ttl) = ttl.filter(|_| i + 1 == tools.len()) {
                tool["cache_control"] = cache_control(ttl);
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
pub(crate) fn uses_budget_thinking(model: &str) -> bool {
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
        // A level added later: high's budget until it has its own.
        _ => 16384,
    }
}

/// The smallest thinking budget Anthropic takes.
const MIN_BUDGET: u32 = 1024;

/// `thinking` (+ `output_config`) for `effort` in a request capped at
/// `max_tokens`; empty for none or `off`.
///
/// A model that [`uses_budget_thinking`] gets a token budget (2,048 for low
/// up to 31,999 for max), cut to below `max_tokens` as Anthropic requires;
/// a cap of 1,024 or less leaves no room for the smallest budget it takes,
/// and that request thinks not at all. Any newer model gets adaptive
/// thinking with the effort level in `output_config`.
pub(crate) fn thinking_params(
    model: &str,
    effort: Option<Effort>,
    max_tokens: u32,
) -> Map<String, Value> {
    let mut out = Map::new();
    let Some(effort) = effort.filter(|e| *e != Effort::Off) else {
        return out;
    };
    if uses_budget_thinking(model) {
        let budget = budget(effort).min(max_tokens.saturating_sub(1));
        if budget >= MIN_BUDGET {
            out.insert(
                "thinking".into(),
                json!({"type": "enabled", "budget_tokens": budget}),
            );
        }
    } else {
        out.insert("thinking".into(), json!({"type": "adaptive"}));
        out.insert("output_config".into(), json!({"effort": effort}));
    }
    out
}
