//! Chat Completions stream accumulation, as plain functions over a small state
//! object so it can be tested with synthetic chunks and no network.
//!
//! Providers disagree on where reasoning lives (`reasoning` on OpenRouter,
//! `reasoning_content` on DeepSeek-style servers) and stream tool calls as
//! index-keyed fragments; this file absorbs both.

use std::collections::BTreeMap;

use kern_core::{
    messages::{AssistantMessage, AssistantPart, StopReason, ToolCall},
    provider::ProviderEvent,
};
use serde_json::{Map, Value};

use crate::{
    driver::{count, parse_args, str_field, text_of},
    http::random_hex,
    web_search::SearchAcc,
};

fn stop_reason(reason: &str) -> StopReason {
    match reason {
        "tool_calls" | "function_call" => StopReason::ToolUse,
        "length" => StopReason::MaxTokens,
        "content_filter" => StopReason::Refusal,
        _ => StopReason::EndTurn,
    }
}

#[derive(Debug)]
struct ToolAcc {
    id: String,
    name: String,
    args: String,
}

#[derive(Debug, Default)]
pub struct StreamState {
    text: String,
    thinking: String,
    tools: BTreeMap<i64, ToolAcc>,
    /// reasoning_details items by their stream `index`; fragments of one index merge.
    reasoning_details: BTreeMap<i64, Map<String, Value>>,
    /// `reasoning_content` alone (DeepSeek-style servers), kept apart from `thinking`
    /// because those servers want exactly this field back on the next turn.
    reasoning_content: String,
    /// A provider-side web search's citations and count.
    search: SearchAcc,
}

/// Fields streamed a token at a time; everything else (signature, data, id, format) arrives whole.
const APPENDED_DETAIL_FIELDS: [&str; 2] = ["text", "summary"];

/// OpenRouter streams one reasoning item as many fragments sharing an `index`.
/// Kept as separate items, they were replayed one word per item, and the model
/// started writing that way. Merge by index, concatenating text.
fn merge_reasoning_detail(st: &mut StreamState, raw: &Value) {
    let Some(item) = raw.as_object() else { return };
    let index = item
        .get("index")
        .and_then(Value::as_i64)
        .unwrap_or(st.reasoning_details.len() as i64);
    match st.reasoning_details.get_mut(&index) {
        Some(into) => merge_fields(into, item),
        None => {
            let mut fresh = item.clone();
            fresh.insert("index".into(), Value::from(index));
            st.reasoning_details.insert(index, fresh);
        }
    }
}

fn merge_fields(into: &mut Map<String, Value>, item: &Map<String, Value>) {
    for (key, value) in item {
        if value.is_null() {
            continue;
        }
        let appended = match (into.get(key), value) {
            (Some(Value::String(prev)), Value::String(add))
                if APPENDED_DETAIL_FIELDS.contains(&key.as_str()) =>
            {
                Some(format!("{prev}{add}"))
            }
            _ => None,
        };
        into.insert(
            key.clone(),
            appended.map_or_else(|| value.clone(), Value::String),
        );
    }
}

fn apply_reasoning(d: &Value, st: &mut StreamState, out: &mut Vec<ProviderEvent>) {
    if let Some(details) = d.get("reasoning_details").and_then(Value::as_array) {
        for detail in details {
            merge_reasoning_detail(st, detail);
        }
    }
    if let Some(rc) = text_of(d.get("reasoning_content")) {
        st.reasoning_content.push_str(rc);
    }
    let Some(text) = text_of(d.get("reasoning")).or_else(|| text_of(d.get("reasoning_content")))
    else {
        return;
    };
    st.thinking.push_str(text);
    out.push(ProviderEvent::ThinkingDelta(text.to_string()));
}

fn apply_content(d: &Value, st: &mut StreamState, out: &mut Vec<ProviderEvent>) {
    if let Some(text) = text_of(d.get("content")) {
        st.text.push_str(text);
        out.push(ProviderEvent::TextDelta(text.to_string()));
    }
}

/// The first fragment for an index carries id + name; later ones append arguments.
fn apply_tool_delta(tc: &Value, st: &mut StreamState, out: &mut Vec<ProviderEvent>) {
    let index = tc.get("index").and_then(Value::as_i64).unwrap_or(0);
    let function = tc.get("function").unwrap_or(&Value::Null);
    if let Some(existing) = st.tools.get_mut(&index) {
        // Some servers send the name only on a later fragment.
        if existing.name.is_empty() {
            existing.name = str_field(function, "name");
        }
        existing.args.push_str(&str_field(function, "arguments"));
        return;
    }
    let acc = ToolAcc {
        id: str_field(tc, "id"),
        name: str_field(function, "name"),
        args: str_field(function, "arguments"),
    };
    out.push(ProviderEvent::ToolCallStart {
        id: acc.id.clone(),
        name: acc.name.clone(),
    });
    st.tools.insert(index, acc);
}

fn apply_usage(u: &Value, msg: &mut AssistantMessage, st: &mut StreamState) {
    st.search.apply_usage(u);
    let cached = count(u.pointer("/prompt_tokens_details/cached_tokens"));
    let written = count(u.pointer("/prompt_tokens_details/cache_write_tokens"));
    let prompt = count(u.get("prompt_tokens"));
    msg.usage.input = prompt.saturating_sub(cached + written);
    msg.usage.cache_read = cached;
    msg.usage.cache_write = written;
    msg.usage.output = count(u.get("completion_tokens"));
    msg.usage.reasoning = count(u.pointer("/completion_tokens_details/reasoning_tokens"));
    if let Some(cost) = u.get("cost").and_then(Value::as_f64) {
        msg.usage.cost_usd = Some(cost);
    }
}

/// Fold one chunk into the message + state; pushes the live events it produced.
pub fn apply_chunk(
    chunk: &Value,
    msg: &mut AssistantMessage,
    st: &mut StreamState,
    out: &mut Vec<ProviderEvent>,
) {
    if let Some(model) = text_of(chunk.get("model")) {
        msg.model = model.to_string();
    }
    if let Some(u) = chunk.get("usage").filter(|u| u.is_object()) {
        apply_usage(u, msg, st);
    }
    let Some(choice) = chunk.pointer("/choices/0") else {
        return;
    };
    let d = choice.get("delta").unwrap_or(&Value::Null);
    st.search.apply_annotations(d.get("annotations"));
    apply_reasoning(d, st, out);
    apply_content(d, st, out);
    for tc in d
        .get("tool_calls")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        apply_tool_delta(tc, st, out);
    }
    if let Some(reason) = text_of(choice.get("finish_reason")) {
        msg.stop_reason = stop_reason(reason);
    }
}

fn tool_part(t: ToolAcc) -> AssistantPart {
    let id = if t.id.is_empty() {
        format!("call_{}", random_hex(8))
    } else {
        t.id
    };
    AssistantPart::ToolCall(ToolCall {
        id,
        name: t.name,
        input: parse_args(&t.args),
    })
}

/// Close the message. A failed stream keeps text/thinking but never half-built tool calls.
pub fn finish_message(msg: &mut AssistantMessage, st: StreamState, failed: bool) {
    if !st.thinking.is_empty() {
        msg.content.push(AssistantPart::Thinking {
            thinking: st.thinking,
            signature: None,
            redacted: None,
        });
    }
    if !st.text.is_empty() {
        msg.content.push(AssistantPart::Text { text: st.text });
    }
    let mut data = Map::new();
    if !st.reasoning_details.is_empty() {
        let items = st
            .reasoning_details
            .into_values()
            .map(Value::Object)
            .collect();
        data.insert("reasoning_details".into(), Value::Array(items));
    }
    if !st.reasoning_content.is_empty() {
        data.insert(
            "reasoning_content".into(),
            Value::String(st.reasoning_content),
        );
    }
    if !data.is_empty() {
        msg.provider_data = Some(Value::Object(data));
    }
    msg.web_search = st.search.into_use();
    if failed {
        return;
    }
    let had_tools = !st.tools.is_empty();
    msg.content.extend(st.tools.into_values().map(tool_part));
    if msg.stop_reason == StopReason::EndTurn && had_tools {
        msg.stop_reason = StopReason::ToolUse;
    }
}
