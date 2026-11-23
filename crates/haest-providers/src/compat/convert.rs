//! Neutral messages -> OpenAI Chat Completions params. Pure.
//!
//! Chat Completions is the lingua franca: OpenRouter and every
//! "OpenAI-compatible" server (vLLM, LM Studio, Ollama, z.ai, ...) speak it.
//! Flavor-specific extras (OpenRouter prompt caching + reasoning replay) are
//! applied here and nowhere else.

use haest_core::{
    cache::{CachePlan, CacheTtl},
    catalog::Reasoning,
    messages::{AssistantMessage, Message},
    provider::{Effort, ProviderRequest, ToolSpec, nearest_accepted_effort},
};
use serde_json::{Map, Value, json};

use super::{CompatFlavor, CompatOptions};
use crate::{
    anthropic::uses_budget_thinking,
    cache::cache_control,
    openai_media::{CHAT, with_tool_media},
    result_text,
    routing::RoutingScopes,
    send::is_openai_api,
};

/// An Anthropic model on OpenRouter: it takes `cache_control` with a TTL,
/// and on a function tool too, and a cache pre-warm.
pub(super) fn openrouter_anthropic(flavor: CompatFlavor, model: &str) -> bool {
    flavor == CompatFlavor::OpenRouter && model.starts_with("anthropic/")
}

/// Whether OpenRouter's Anthropic `model` (`anthropic/claude-sonnet-4.5`,
/// a `:batch` variant too) thinks on a token budget at an `effort` above
/// off: the Claude models before adaptive thinking, the same ones
/// Anthropic's own API sends a budget for ([`uses_budget_thinking`] on the
/// id with its dots as dashes). OpenRouter derives that budget from
/// `max_tokens` and leaves the thinking out when the cap is too small for
/// it, so a pre-warm capped at one token renders another prompt than the
/// real requests do.
pub(super) fn openrouter_budget_thinking(model: &str, effort: Option<Effort>) -> bool {
    let thinks = effort.is_some_and(|e| e != Effort::Off);
    let id = model.strip_prefix("anthropic/").unwrap_or(model);
    let id = id.split_once(':').map_or(id, |(id, _)| id);
    thinks && uses_budget_thinking(&id.replace('.', "-"))
}

/// The `cache_control` a breakpoint for `model` carries, or None where the
/// model caches by itself. OpenRouter passes it through to the providers
/// that need explicit breakpoints: Anthropic's, with `ttl` (OpenRouter
/// documents `5m` and `1h` for them), and Google's, at the default.
fn cache_marker(flavor: CompatFlavor, model: &str, ttl: CacheTtl) -> Option<Value> {
    if openrouter_anthropic(flavor, model) {
        Some(cache_control(ttl))
    } else if flavor == CompatFlavor::OpenRouter && model.starts_with("google/") {
        Some(cache_control(CacheTtl::FiveMinutes))
    } else {
        None
    }
}

/// The last of `tools` (the function tools, before any server tool) with a
/// breakpoint that lives `ttl`, for an Anthropic model on OpenRouter: the
/// tool list then reads from the cache when the system prompt changes but
/// the tools do not, as on Anthropic's own API. None: no breakpoint.
pub(super) fn mark_last_tool(
    tools: &mut [Value],
    flavor: CompatFlavor,
    model: &str,
    ttl: Option<CacheTtl>,
) {
    if let (Some(last), Some(ttl)) = (tools.last_mut(), ttl)
        && openrouter_anthropic(flavor, model)
    {
        last["cache_control"] = cache_control(ttl);
    }
}

/// OpenRouter's documented limit on a `session_id`.
const SESSION_ID_MAX: usize = 256;

/// The request's cache keys as the server named by `opts` takes them:
/// `cache_key` as OpenRouter's `session_id` (its sticky routing key, at most
/// 256 characters; a longer key is not sent), and `partition_key` as OpenAI's
/// own API's `prompt_cache_key`. Nothing for any other server, which may
/// refuse a field it does not know; OpenAI's own API never gets `cache_key`,
/// since there a key partitions the cache instead of routing to it.
pub(super) fn cache_key_extras(opts: &CompatOptions, req: &ProviderRequest) -> Map<String, Value> {
    let mut extras = Map::new();
    match (opts.flavor, &req.cache_key, &req.cache.partition_key) {
        (CompatFlavor::OpenRouter, Some(key), _) if key.chars().count() <= SESSION_ID_MAX => {
            extras.insert("session_id".into(), json!(key));
        }
        (CompatFlavor::OpenRouter, _, _) => {}
        (_, _, Some(key)) if is_openai_api(opts.base_url()) => {
            extras.insert("prompt_cache_key".into(), json!(key));
        }
        _ => {}
    }
    extras
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

/// `system` and `messages` as the Chat Completions `messages` array, the
/// system prompt first.
///
/// An assistant message from the provider named `provider_id` carries its
/// reasoning back (`reasoning_details`, `reasoning_content`); one from any
/// other provider goes as text and tool calls only. A tool result's media,
/// which a `tool` message cannot hold, follows the run of tool results as a
/// user message.
///
/// On OpenRouter the breakpoints of `cache` go in (a [`CacheTtl`] is the
/// standard plan at that TTL): an `anthropic/` model's on the system prompt,
/// the newest user or tool message and the messages the plan chose, each
/// with its TTL; a `google/` model's on the system prompt and the newest
/// user or tool message only, at five minutes ([`cache_marker`]). A
/// breakpoint goes on a message's last text part, so a chosen assistant
/// message, or one with no text, gets none.
pub(crate) fn to_chat_messages(
    system: &str,
    messages: &[Message],
    flavor: CompatFlavor,
    model: &str,
    cache: impl Into<CachePlan>,
    provider_id: &str,
) -> Vec<Value> {
    let cache = cache.into();
    let marker = |ttl: Option<CacheTtl>| ttl.and_then(|ttl| cache_marker(flavor, model, ttl));
    let mut out = Vec::new();
    let system_content = match marker(cache.system) {
        Some(marker) => json!([{"type": "text", "text": system, "cache_control": marker}]),
        None => json!(system),
    };
    out.push(json!({"role": "system", "content": system_content}));
    // Where each message landed in `out`, for the plan's chosen ones.
    let mut landed = Vec::with_capacity(messages.len());
    with_tool_media(messages, &CHAT, &mut out, |m, out| {
        let before = out.len();
        match m {
            Message::User(u) => out
                .push(json!({"role": "user", "content": CHAT.user_content(&u.content, &u.media)})),
            Message::Tool(t) => out.push(
                json!({"role": "tool", "tool_call_id": t.tool_call_id, "content": result_text(t)}),
            ),
            Message::Assistant(a) if !a.content.is_empty() => {
                out.push(assistant_msg(a, provider_id))
            }
            Message::Assistant(_) => {}
        }
        landed.push((out.len() > before).then(|| out.len() - 1));
    });
    if openrouter_anthropic(flavor, model) {
        for (index, ttl) in &cache.messages {
            if let Some(Some(at)) = landed.get(*index) {
                mark_cacheable(&mut out[*at], &cache_control(*ttl));
            }
        }
    }
    if let Some(marker) = marker(cache.history) {
        for m in out.iter_mut().skip(1).rev() {
            if mark_cacheable(m, &marker) {
                break;
            }
        }
    }
    out
}

/// `m` with `marker` on its last text part, when it is a user or tool
/// message that has one (a string content becomes one text part); whether
/// it took it.
fn mark_cacheable(m: &mut Value, marker: &Value) -> bool {
    let role = m.get("role").and_then(Value::as_str);
    if role != Some("user") && role != Some("tool") {
        return false;
    }
    match m.get_mut("content") {
        Some(Value::String(text)) => {
            let text = std::mem::take(text);
            m["content"] = json!([{"type": "text", "text": text, "cache_control": marker}]);
            true
        }
        Some(Value::Array(parts)) => {
            let last_text = parts
                .iter_mut()
                .rev()
                .find(|p| p.get("type").and_then(Value::as_str) == Some("text"));
            match last_text {
                Some(part) => {
                    part["cache_control"] = marker.clone();
                    true
                }
                None => false,
            }
        }
        _ => false,
    }
}

/// `tools` as Chat Completions `function` tools.
pub(crate) fn to_chat_tools(tools: &[ToolSpec]) -> Vec<Value> {
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

/// Extra body fields per flavor; empty for [`CompatFlavor::Generic`].
///
/// For OpenRouter: `usage: {include: true}` (the cost comes back), `reasoning`
/// for `effort` (`{enabled: false}` for off, `{effort}` otherwise, nothing for
/// None so the model uses its default) and `provider`, what `routing` resolves
/// to for `model` ([`crate::routing`]).
///
/// Every haest level is an OpenRouter level by the same name, so it goes through
/// as is; one a model does not list is accepted, not refused. `off` for a catalog
/// model whose reasoning is mandatory is fitted before it gets here (haest-core `effort`).
pub(crate) fn flavor_extras(
    flavor: CompatFlavor,
    effort: Option<Effort>,
    model: &str,
    routing: Option<&RoutingScopes>,
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
    if let Some(provider) = routing.and_then(|r| r.provider_prefs(model)) {
        extras.insert("provider".into(), json!(provider));
    }
    extras
}

/// The effort dialect of a direct OpenAI-compatible provider: a top-level
/// `reasoning_effort` that takes only the levels the provider lists and 400s on
/// anything else (Paper Instruments: low / high / max, no `medium`, no off).
/// Empty when either is absent or nothing fits.
pub(super) fn reasoning_effort_extras(
    dialect: Option<&Reasoning>,
    effort: Option<Effort>,
) -> Map<String, Value> {
    let level = dialect
        .zip(effort)
        .and_then(|(dialect, effort)| nearest_accepted_effort(effort, dialect));
    level
        .map(|level| ("reasoning_effort".to_string(), json!(level)))
        .into_iter()
        .collect()
}
