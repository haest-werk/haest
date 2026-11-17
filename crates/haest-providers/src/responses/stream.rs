//! Folding the Responses API's SSE events into one assistant message.
//!
//! Output items arrive keyed by `output_index` and are folded into parts in
//! that order, because a replayed reasoning item must sit right before the
//! item it produced. `output_item.done` carries each item's final form
//! (encrypted reasoning, complete arguments) and wins over the deltas.

use std::collections::BTreeMap;

use haest_core::{
    messages::{AssistantMessage, AssistantPart, StopReason, ToolCall},
    provider::{ProviderEvent, mark_stream_failed},
};
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use crate::{
    driver::{Step, StreamFold, count, openai_json, parse_args, str_field, text_of},
    sse::SseEvent,
};

struct ItemAcc {
    part: AssistantPart,
    args: String,
}

pub(crate) struct ResponsesFold {
    msg: AssistantMessage,
    items: BTreeMap<u64, ItemAcc>,
    /// Set once a message item carried a refusal instead of text.
    refused: bool,
    incomplete: Option<String>,
}

impl ResponsesFold {
    pub(super) fn new(provider_id: &str, model: &str) -> Self {
        Self {
            msg: AssistantMessage::start(provider_id, model),
            items: BTreeMap::new(),
            refused: false,
            incomplete: None,
        }
    }

    fn apply_response(&mut self, r: &Value) {
        if let Some(model) = text_of(r.get("model")) {
            self.msg.model = model.to_string();
        }
        let Some(u) = r.get("usage").filter(|u| u.is_object()) else {
            return;
        };
        let cached = count(u.pointer("/input_tokens_details/cached_tokens"));
        let written = count(u.pointer("/input_tokens_details/cache_write_tokens"));
        let usage = &mut self.msg.usage;
        usage.input = count(u.get("input_tokens")).saturating_sub(cached.saturating_add(written));
        usage.cache_read = cached;
        usage.cache_write = written;
        usage.output = count(u.get("output_tokens"));
        usage.reasoning = count(u.pointer("/output_tokens_details/reasoning_tokens"));
    }

    fn append_to(&mut self, index: u64, thinking: bool, delta: &str) -> Option<ProviderEvent> {
        match (&mut self.items.get_mut(&index)?.part, thinking) {
            (AssistantPart::Text { text }, false) => {
                text.push_str(delta);
                Some(ProviderEvent::TextDelta(delta.to_string()))
            }
            (AssistantPart::Thinking { thinking, .. }, true) => {
                thinking.push_str(delta);
                Some(ProviderEvent::ThinkingDelta(delta.to_string()))
            }
            _ => None,
        }
    }

    fn item_added(&mut self, index: u64, item: &Value) -> Option<ProviderEvent> {
        let part = match item.get("type").and_then(Value::as_str)? {
            "reasoning" => AssistantPart::Thinking {
                thinking: String::new(),
                signature: None,
                redacted: None,
            },
            "message" => AssistantPart::Text {
                text: String::new(),
            },
            "function_call" => AssistantPart::ToolCall(ToolCall {
                id: str_field(item, "call_id"),
                name: str_field(item, "name"),
                input: Default::default(),
            }),
            _ => return None,
        };
        let start = match &part {
            AssistantPart::ToolCall(c) => Some(ProviderEvent::ToolCallStart {
                id: c.id.clone(),
                name: c.name.clone(),
            }),
            _ => None,
        };
        self.items.insert(
            index,
            ItemAcc {
                part,
                args: String::new(),
            },
        );
        start
    }

    /// The final item replaces what the deltas built: it is the replayable form.
    fn item_done(&mut self, index: u64, item: &Value) {
        let Some(acc) = self.items.get_mut(&index) else {
            return;
        };
        match (item.get("type").and_then(Value::as_str), &mut acc.part) {
            (
                Some("reasoning"),
                AssistantPart::Thinking {
                    thinking,
                    signature,
                    ..
                },
            ) => {
                let summary: Vec<&str> = item
                    .get("summary")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter_map(|s| s.get("text").and_then(Value::as_str))
                    .collect();
                let summary = summary.join("\n\n");
                if !summary.is_empty() {
                    *thinking = summary;
                }
                if let Some(enc) = text_of(item.get("encrypted_content")) {
                    *signature = Some(enc.to_string());
                }
            }
            (Some("message"), AssistantPart::Text { text }) => {
                let content = item.get("content").and_then(Value::as_array);
                let Some(content) = content.filter(|c| !c.is_empty()) else {
                    return;
                };
                let piece = |c: &Value| {
                    let key = if c.get("type").and_then(Value::as_str) == Some("output_text") {
                        "text"
                    } else {
                        "refusal"
                    };
                    c.get(key).and_then(Value::as_str).unwrap_or("").to_string()
                };
                *text = content.iter().map(piece).collect();
                self.refused |= content
                    .iter()
                    .any(|c| c.get("type").and_then(Value::as_str) == Some("refusal"));
            }
            (Some("function_call"), AssistantPart::ToolCall(call)) => {
                if let Some(args) = item.get("arguments").and_then(Value::as_str) {
                    acc.args = args.to_string();
                }
                if let Some(id) = text_of(item.get("call_id")) {
                    call.id = id.to_string();
                }
            }
            _ => {}
        }
    }
}

fn keep(p: &AssistantPart) -> bool {
    match p {
        AssistantPart::Text { text } => !text.is_empty(),
        AssistantPart::Thinking {
            thinking,
            signature,
            ..
        } => !thinking.is_empty() || signature.as_deref().is_some_and(|s| !s.is_empty()),
        _ => true,
    }
}

impl StreamFold for ResponsesFold {
    fn on_event(&mut self, sse: &SseEvent, out: &mut Vec<ProviderEvent>) -> Result<Step, String> {
        let ev = openai_json(sse)?;
        let kind = ev
            .get("type")
            .and_then(Value::as_str)
            .or(sse.event.as_deref())
            .unwrap_or("");
        let index = count(ev.get("output_index"));
        let delta = ev.get("delta").and_then(Value::as_str).unwrap_or("");
        let event = match kind {
            "response.output_item.added" => ev.get("item").and_then(|i| self.item_added(index, i)),
            "response.output_text.delta" => self.append_to(index, false, delta),
            "response.reasoning_summary_text.delta" => self.append_to(index, true, delta),
            "response.reasoning_summary_part.added" if count(ev.get("summary_index")) > 0 => {
                self.append_to(index, true, "\n\n")
            }
            "response.function_call_arguments.delta" => {
                if let Some(acc) = self.items.get_mut(&index) {
                    acc.args.push_str(delta);
                }
                None
            }
            "response.output_item.done" => {
                if let Some(item) = ev.get("item") {
                    self.item_done(index, item);
                }
                None
            }
            "response.completed" => {
                self.apply_response(ev.get("response").unwrap_or(&Value::Null));
                None
            }
            "response.incomplete" => {
                let r = ev.get("response").unwrap_or(&Value::Null);
                self.apply_response(r);
                let reason = r
                    .pointer("/incomplete_details/reason")
                    .and_then(Value::as_str);
                self.incomplete = Some(reason.unwrap_or("unknown").to_string());
                None
            }
            "response.failed" => {
                let message = ev
                    .pointer("/response/error/message")
                    .and_then(Value::as_str);
                return Err(message.unwrap_or("response failed").to_string());
            }
            "error" => {
                let message = ev.get("message").and_then(Value::as_str);
                return Err(message.unwrap_or("stream error").to_string());
            }
            _ => None,
        };
        out.extend(event);
        Ok(Step::Continue)
    }

    /// Parts in output order. A failed stream never keeps a half-built tool call.
    fn finish(mut self, failure: Option<&str>, cancel: &CancellationToken) -> AssistantMessage {
        let complete = failure.is_none();
        let parts: Vec<AssistantPart> = std::mem::take(&mut self.items)
            .into_values()
            .map(|acc| match acc.part {
                AssistantPart::ToolCall(mut c) => {
                    c.input = parse_args(&acc.args);
                    AssistantPart::ToolCall(c)
                }
                part => part,
            })
            .collect();
        let has_tool_call = parts
            .iter()
            .any(|p| matches!(p, AssistantPart::ToolCall(_)));
        self.msg.content = parts
            .into_iter()
            .filter(|p| keep(p) && (complete || !matches!(p, AssistantPart::ToolCall(_))))
            .collect();
        if let Some(err) = failure {
            mark_stream_failed(&mut self.msg, cancel, err);
            return self.msg;
        }
        let incomplete = match self.incomplete.as_deref() {
            Some("max_output_tokens") => Some(StopReason::MaxTokens),
            Some("content_filter") => Some(StopReason::Refusal),
            _ => None,
        };
        if let Some(reason) = incomplete {
            self.msg.stop_reason = reason;
        } else if has_tool_call {
            self.msg.stop_reason = StopReason::ToolUse;
        } else if self.refused {
            self.msg.stop_reason = StopReason::Refusal;
        }
        self.msg
    }
}
