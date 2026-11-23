//! Folding the Messages API's SSE events into one assistant message, or a
//! whole non-streamed response (a cache pre-warm's) into one.

use std::collections::BTreeMap;

use haest_core::{
    catalog::ModelPricing,
    messages::{AssistantMessage, AssistantPart, StopReason, ToolCall, Usage},
    provider::{ProviderEvent, book_priced_cost, mark_stream_failed},
};
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use crate::{
    driver::{JsonFold, Step, StreamFold, count, parse_object, str_field, text_of},
    sse::SseEvent,
};

struct BlockAcc {
    part: AssistantPart,
    json: String,
}

pub(crate) struct AnthropicFold {
    msg: AssistantMessage,
    blocks: BTreeMap<u64, BlockAcc>,
    /// The configured price of the model asked for: the API reports no cost.
    price: Option<ModelPricing>,
}

impl AnthropicFold {
    pub(super) fn new(provider_id: &str, model: &str, price: Option<ModelPricing>) -> Self {
        Self {
            msg: AssistantMessage::start(provider_id, model),
            blocks: BTreeMap::new(),
            price,
        }
    }
}

/// `v` as a token count, when it is a number.
fn number(v: Option<&Value>) -> Option<u64> {
    v.filter(|v| v.is_number()).map(|v| count(Some(v)))
}

/// The input-side counts `u` carries over what `usage` holds: a
/// `message_start`'s, a `message_delta`'s (each count it carries is the
/// call's total so far) or a whole response's. A count it leaves out stays.
/// Of the cache write, `cache_creation.ephemeral_1h_input_tokens` lived an
/// hour; never more than the write itself.
fn read_input_usage(usage: &mut Usage, u: &Value) {
    let fields = [
        (&mut usage.input, u.get("input_tokens")),
        (&mut usage.cache_read, u.get("cache_read_input_tokens")),
        (&mut usage.cache_write, u.get("cache_creation_input_tokens")),
        (
            &mut usage.cache_write_1h,
            u.pointer("/cache_creation/ephemeral_1h_input_tokens"),
        ),
    ];
    for (field, v) in fields {
        if let Some(n) = number(v) {
            *field = n;
        }
    }
    usage.cache_write_1h = usage.cache_write_1h.min(usage.cache_write);
}

/// The output-side counts of a `message_delta`'s or a whole response's
/// usage `u`: the output when it says, the reasoning in it.
fn read_output_usage(usage: &mut Usage, u: &Value) {
    if let Some(out) = number(u.get("output_tokens")) {
        usage.output = out;
    }
    usage.reasoning = count(u.pointer("/output_tokens_details/thinking_tokens"));
}

fn stop_reason(reason: Option<&str>) -> StopReason {
    match reason.unwrap_or("end_turn") {
        "tool_use" => StopReason::ToolUse,
        "max_tokens" => StopReason::MaxTokens,
        "refusal" => StopReason::Refusal,
        _ => StopReason::EndTurn,
    }
}

/// The part a `content_block_start` opens, or None for a block haest does not keep.
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
        read_input_usage(&mut self.msg.usage, u);
        if let Some(model) = text_of(ev.pointer("/message/model")) {
            self.msg.model = model.to_string();
        }
    }

    fn message_delta(&mut self, ev: &Value) {
        let reason = ev.pointer("/delta/stop_reason").and_then(Value::as_str);
        self.msg.stop_reason = stop_reason(reason);
        let u = ev.get("usage").unwrap_or(&Value::Null);
        read_input_usage(&mut self.msg.usage, u);
        read_output_usage(&mut self.msg.usage, u);
    }
}

/// A whole Messages API response (`{"type": "message", ...}`): its model,
/// usage, stop reason and content blocks, as the stream's events would have
/// carried them. An `{"type": "error"}` body fails the call.
impl JsonFold for AnthropicFold {
    fn on_response(&mut self, body: Value) -> Result<(), String> {
        if body.get("type").and_then(Value::as_str) == Some("error") {
            return Err(error_text(&body));
        }
        if let Some(model) = text_of(body.get("model")) {
            self.msg.model = model.to_string();
        }
        let u = body.get("usage").unwrap_or(&Value::Null);
        read_input_usage(&mut self.msg.usage, u);
        read_output_usage(&mut self.msg.usage, u);
        self.msg.stop_reason = stop_reason(body.get("stop_reason").and_then(Value::as_str));
        let blocks = body.get("content").and_then(Value::as_array);
        for (index, block) in blocks.into_iter().flatten().enumerate() {
            if let Some(part) = start_part(block) {
                let json = String::new();
                self.blocks.insert(index as u64, BlockAcc { part, json });
            }
        }
        Ok(())
    }
}

/// An `error` event's or body's message, as the SDK says it.
fn error_text(ev: &Value) -> String {
    let err = ev.get("error").unwrap_or(ev);
    err.get("message")
        .or_else(|| err.get("type"))
        .and_then(Value::as_str)
        .unwrap_or("stream error")
        .to_string()
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
            "error" => return Err(error_text(&ev)),
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

    /// Blocks in index order. A failed stream never keeps a half-built tool
    /// call. The cost is the configured price's, the API reporting none.
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
        book_priced_cost(&mut self.msg.usage, self.price.as_ref());
        self.msg
    }
}
