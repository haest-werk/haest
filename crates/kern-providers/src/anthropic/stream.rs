//! Folding the Messages API's SSE events into one assistant message.

use std::collections::BTreeMap;

use kern_core::{
    messages::{AssistantMessage, AssistantPart, StopReason, ToolCall},
    provider::{ProviderEvent, mark_stream_failed},
};
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use crate::{
    driver::{Step, StreamFold, count, parse_object, str_field, text_of},
    sse::SseEvent,
};

struct BlockAcc {
    part: AssistantPart,
    json: String,
}

pub(crate) struct AnthropicFold {
    msg: AssistantMessage,
    blocks: BTreeMap<u64, BlockAcc>,
}

impl AnthropicFold {
    pub fn new(model: &str) -> Self {
        Self {
            msg: AssistantMessage::start("anthropic", model),
            blocks: BTreeMap::new(),
        }
    }
}

fn stop_reason(reason: Option<&str>) -> StopReason {
    match reason.unwrap_or("end_turn") {
        "tool_use" => StopReason::ToolUse,
        "max_tokens" => StopReason::MaxTokens,
        "refusal" => StopReason::Refusal,
        _ => StopReason::EndTurn,
    }
}

/// The part a `content_block_start` opens, or None for a block kern does not keep.
fn start_part(block: &Value) -> Option<AssistantPart> {
    Some(match block.get("type").and_then(Value::as_str)? {
        "text" => AssistantPart::Text {
            text: str_field(block, "text"),
        },
        "thinking" => AssistantPart::Thinking {
            thinking: str_field(block, "thinking"),
            signature: block
                .get("signature")
                .and_then(Value::as_str)
                .map(str::to_string),
            redacted: None,
        },
        "redacted_thinking" => AssistantPart::Thinking {
            thinking: String::new(),
            signature: None,
            redacted: Some(str_field(block, "data")),
        },
        "tool_use" => AssistantPart::ToolCall(ToolCall {
            id: str_field(block, "id"),
            name: str_field(block, "name"),
            input: block
                .get("input")
                .and_then(Value::as_object)
                .cloned()
                .unwrap_or_default(),
        }),
        _ => return None,
    })
}

/// One delta into its block; the live event it produced, if any.
fn apply_delta(acc: &mut BlockAcc, delta: &Value) -> Option<ProviderEvent> {
    match (delta.get("type").and_then(Value::as_str)?, &mut acc.part) {
        ("text_delta", part) => {
            let text = str_field(delta, "text");
            if let AssistantPart::Text { text: t } = part {
                t.push_str(&text);
            }
            Some(ProviderEvent::TextDelta(text))
        }
        ("thinking_delta", part) => {
            let text = str_field(delta, "thinking");
            if let AssistantPart::Thinking { thinking, .. } = part {
                thinking.push_str(&text);
            }
            Some(ProviderEvent::ThinkingDelta(text))
        }
        ("signature_delta", AssistantPart::Thinking { signature, .. }) => {
            *signature = Some(str_field(delta, "signature"));
            None
        }
        ("input_json_delta", _) => {
            acc.json.push_str(&str_field(delta, "partial_json"));
            None
        }
        _ => None,
    }
}

/// A tool call's streamed input replaces the one its start carried.
fn finish_block(b: BlockAcc) -> AssistantPart {
    match b.part {
        AssistantPart::ToolCall(mut call) if !b.json.is_empty() => {
            call.input = parse_object(&b.json);
            AssistantPart::ToolCall(call)
        }
        part => part,
    }
}

impl AnthropicFold {
    fn message_start(&mut self, ev: &Value) {
        let u = ev.pointer("/message/usage").unwrap_or(&Value::Null);
        self.msg.usage.input = count(u.get("input_tokens"));
        self.msg.usage.cache_read = count(u.get("cache_read_input_tokens"));
        self.msg.usage.cache_write = count(u.get("cache_creation_input_tokens"));
        if let Some(model) = text_of(ev.pointer("/message/model")) {
            self.msg.model = model.to_string();
        }
    }

    fn message_delta(&mut self, ev: &Value) {
        let reason = ev.pointer("/delta/stop_reason").and_then(Value::as_str);
        self.msg.stop_reason = stop_reason(reason);
        if let Some(out) = ev.pointer("/usage/output_tokens").filter(|v| v.is_number()) {
            self.msg.usage.output = count(Some(out));
        }
        self.msg.usage.reasoning =
            count(ev.pointer("/usage/output_tokens_details/thinking_tokens"));
    }
}

/// The events the fold reads. As in the SDK, any other is skipped unread.
const FOLDED: [&str; 5] = [
    "error",
    "message_start",
    "content_block_start",
    "content_block_delta",
    "message_delta",
];

/// An event whose data is not JSON. One the fold reads fails the stream (an
/// `error` with its text as the server sent it, as the SDK says it); any other
/// -- a proxy's keep-alive, a trailing `[DONE]` -- is skipped.
fn not_json(sse: &SseEvent, e: serde_json::Error) -> Result<Step, String> {
    match sse.event.as_deref() {
        Some("error") => Err(sse.data.clone()),
        Some(kind) if FOLDED.contains(&kind) => {
            Err(format!("could not parse a stream event as JSON: {e}"))
        }
        _ => Ok(Step::Continue),
    }
}

impl StreamFold for AnthropicFold {
    fn on_event(&mut self, sse: &SseEvent, out: &mut Vec<ProviderEvent>) -> Result<Step, String> {
        let ev: Value = match serde_json::from_str(&sse.data) {
            Ok(ev) => ev,
            Err(e) => return not_json(sse, e),
        };
        let kind = ev
            .get("type")
            .and_then(Value::as_str)
            .or(sse.event.as_deref())
            .unwrap_or("");
        match kind {
            "error" => {
                let err = ev.get("error").unwrap_or(&ev);
                let message = err
                    .get("message")
                    .or_else(|| err.get("type"))
                    .and_then(Value::as_str)
                    .unwrap_or("stream error");
                return Err(message.to_string());
            }
            "message_start" => self.message_start(&ev),
            "content_block_start" => {
                let index = count(ev.get("index"));
                let Some(part) = ev.get("content_block").and_then(start_part) else {
                    return Ok(Step::Continue);
                };
                if let AssistantPart::ToolCall(c) = &part {
                    out.push(ProviderEvent::ToolCallStart {
                        id: c.id.clone(),
                        name: c.name.clone(),
                    });
                }
                self.blocks.insert(
                    index,
                    BlockAcc {
                        part,
                        json: String::new(),
                    },
                );
            }
            "content_block_delta" => {
                let acc = self.blocks.get_mut(&count(ev.get("index")));
                let delta = ev.get("delta");
                if let (Some(acc), Some(delta)) = (acc, delta) {
                    out.extend(apply_delta(acc, delta));
                }
            }
            "message_delta" => self.message_delta(&ev),
            _ => {}
        }
        Ok(Step::Continue)
    }

    /// Blocks in index order. A failed stream never keeps a half-built tool call.
    fn finish(mut self, failure: Option<&str>, cancel: &CancellationToken) -> AssistantMessage {
        let complete = failure.is_none();
        self.msg.content = std::mem::take(&mut self.blocks)
            .into_values()
            .map(finish_block)
            .filter(|p| complete || !matches!(p, AssistantPart::ToolCall(_)))
            .collect();
        if let Some(err) = failure {
            mark_stream_failed(&mut self.msg, cancel, err);
        }
        self.msg
    }
}
