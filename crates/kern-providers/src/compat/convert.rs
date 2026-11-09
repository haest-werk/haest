//! Neutral messages -> OpenAI Chat Completions params. Pure.
//!
//! Chat Completions is the lingua franca: OpenRouter and every
//! "OpenAI-compatible" server (vLLM, LM Studio, Ollama, z.ai, ...) speak it.
//! Flavor-specific extras (OpenRouter prompt caching + reasoning replay) are
//! applied here and nowhere else.

use kern_core::{
    messages::{AssistantMessage, Message},
    provider::{Effort, ToolSpec},
};
use serde_json::{Map, Value, json};

use super::CompatFlavor;
use crate::{
    openai_media::{ToolMedia, tool_media_message, user_content},
    routing::{RoutingEnv, provider_routing_field},
};

/// OpenRouter passes `cache_control` through to providers that need explicit
/// breakpoints (Anthropic, Gemini); everyone else caches automatically.
fn wants_cache_control(flavor: CompatFlavor, model: &str) -> bool {
    flavor == CompatFlavor::OpenRouter
        && (model.starts_with("anthropic/") || model.starts_with("google/"))
}

fn ephemeral() -> Value {
    json!({"type": "ephemeral"})
}

fn assistant_msg(msg: &AssistantMessage, provider_id: &str) -> Value {
    let text = msg.text();
    let calls: Vec<Value> = msg
        .tool_calls()
        .map(|c| {
            json!({
                "id": c.id,
                "type": "function",
                "function": {"name": c.name, "arguments": Value::Object(c.input.clone()).to_string()},
            })
        })
        .collect();
    let mut out = Map::new();
    out.insert("role".into(), json!("assistant"));
    out.insert(
        "content".into(),
        if text.is_empty() {
            Value::Null
        } else {
            Value::String(text)
        },
    );
    if !calls.is_empty() {
        out.insert("tool_calls".into(), Value::Array(calls));
    }
    if msg.provider == provider_id {
        out.extend(replayed_reasoning(msg.provider_data.as_ref()));
    }
    Value::Object(out)
}

fn truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|f| f != 0.0),
        Value::String(s) => !s.is_empty(),
        Value::Array(_) | Value::Object(_) => true,
    }
}

/// Reasoning state the SAME provider wants back: OpenRouter's `reasoning_details`,
/// a DeepSeek-style server's `reasoning_content` (Paper Instruments documents it).
fn replayed_reasoning(data: Option<&Value>) -> Map<String, Value> {
    let mut out = Map::new();
    let Some(data) = data else { return out };
    if let Some(details) = data.get("reasoning_details").filter(|v| truthy(v)) {
        out.insert("reasoning_details".into(), details.clone());
    }
    if let Some(content) = data
        .get("reasoning_content")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
    {
        out.insert("reasoning_content".into(), json!(content));
    }
    out
}

pub fn to_chat_messages(
    system: &str,
    messages: &[Message],
    flavor: CompatFlavor,
    model: &str,
    provider_id: &str,
) -> Vec<Value> {
    let cache = wants_cache_control(flavor, model);
    let mut out = Vec::new();
    let system_content = if cache {
        json!([{"type": "text", "text": system, "cache_control": ephemeral()}])
    } else {
        json!(system)
    };
    out.push(json!({"role": "system", "content": system_content}));
    // A `tool` message carries text only, so a tool's media rides a user message --
    // AFTER the whole run of tool results: one between two of them orphans the rest.
    let mut tool_media: ToolMedia = Vec::new();
    let flush = |tool_media: &mut ToolMedia, out: &mut Vec<Value>| {
        if !tool_media.is_empty() {
            out.push(tool_media_message(tool_media));
        }
        tool_media.clear();
    };
    for m in messages {
        if !matches!(m, Message::Tool(_)) {
            flush(&mut tool_media, &mut out);
        }
        match m {
            Message::User(u) => {
                out.push(json!({"role": "user", "content": user_content(&u.content, &u.media)}))
            }
            Message::Tool(t) => {
                let content = if t.content.is_empty() {
                    "(empty)"
                } else {
                    &t.content
                };
                out.push(
                    json!({"role": "tool", "tool_call_id": t.tool_call_id, "content": content}),
                );
                if !t.media.is_empty() {
                    tool_media.push((t.tool_name.clone(), t.media.clone()));
                }
            }
            Message::Assistant(a) if !a.content.is_empty() => {
                out.push(assistant_msg(a, provider_id))
            }
            Message::Assistant(_) => {}
        }
    }
    flush(&mut tool_media, &mut out);
    if cache {
        mark_last_user_cacheable(&mut out);
    }
    out
}

fn mark_last_user_cacheable(out: &mut [Value]) {
    for m in out.iter_mut().skip(1).rev() {
        let role = m.get("role").and_then(Value::as_str);
        if role != Some("user") && role != Some("tool") {
            continue;
        }
        match m.get_mut("content") {
            Some(Value::String(text)) => {
                let text = std::mem::take(text);
                m["content"] =
                    json!([{"type": "text", "text": text, "cache_control": ephemeral()}]);
                return;
            }
            Some(Value::Array(parts)) => {
                let last_text = parts
                    .iter_mut()
                    .rev()
                    .find(|p| p.get("type").and_then(Value::as_str) == Some("text"));
                if let Some(part) = last_text {
                    part["cache_control"] = ephemeral();
                    return;
                }
            }
            _ => {}
        }
    }
}

pub fn to_chat_tools(tools: &[ToolSpec]) -> Vec<Value> {
    tools
        .iter()
        .map(|t| {
            json!({
                "type": "function",
                "function": {"name": t.name, "description": t.description, "parameters": t.parameters},
            })
        })
        .collect()
}

/// Extra body fields per flavor. `effort` None = let the model default.
/// Every kern level is an OpenRouter level by the same name, so it goes through
/// as is; one a model does not list is accepted, not refused. `off` for a catalog
/// model whose reasoning is mandatory is fitted before it gets here (kern-core `effort`).
pub fn flavor_extras(
    flavor: CompatFlavor,
    effort: Option<Effort>,
    model: &str,
    routing: Option<&RoutingEnv>,
) -> Map<String, Value> {
    let mut extras = Map::new();
    if flavor != CompatFlavor::OpenRouter {
        return extras;
    }
    extras.insert("usage".into(), json!({"include": true}));
    match effort {
        Some(Effort::Off) => {
            extras.insert("reasoning".into(), json!({"enabled": false}));
        }
        Some(e) => {
            extras.insert("reasoning".into(), json!({"effort": e}));
        }
        None => {}
    }
    // Banned -> `ignore`, favorites -> `order` (crate::routing).
    if let Some(provider) = provider_routing_field(routing, model) {
        extras.insert("provider".into(), json!(provider));
    }
    extras
}
