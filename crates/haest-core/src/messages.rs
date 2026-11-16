//! The conversation as data: [`Message`] and everything inside it.
//!
//! This is what the loop keeps in memory ([`Agent::history`]), what the
//! session JSONL stores, and what each [`Provider`] converts to its own wire
//! format. Nothing here knows about a model vendor or a host.
//!
//! A history the loop wrote has one shape every provider accepts: a
//! [`Message::User`] opens it, each [`Message::Assistant`] that called tools is
//! followed by one [`Message::Tool`] per [`ToolCall`], in call order, and any
//! further user input (steering, the turn guard's nudge) comes after those
//! results.
//!
//! The serde shape is the TypeScript haest's, field for field (camelCase keys,
//! `role` / `type` tags), so a session file written by either haest replays in
//! the other.
//!
//! ```
//! use haest_core::messages::{AssistantMessage, AssistantPart, Message};
//!
//! let mut answer = AssistantMessage::start("anthropic", "claude-x");
//! answer.content.push(AssistantPart::Text { text: "hi".into() });
//! let history = vec![Message::user("say hi"), Message::Assistant(answer)];
//!
//! let line = serde_json::to_string(&history[0]).unwrap();
//! assert_eq!(line, r#"{"role":"user","content":"say hi"}"#);
//! ```
//!
//! [`Agent::history`]: crate::Agent::history
//! [`Provider`]: crate::Provider

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// One block of an [`AssistantMessage`], in the order the model produced it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AssistantPart {
    /// Text the model wrote for the human.
    Text {
        /// The text.
        text: String,
    },
    /// The model's reasoning, where the provider exposes it.
    Thinking {
        /// The reasoning as streamed; empty when the provider only sent a
        /// signature or a redacted payload.
        thinking: String,
        /// Opaque token that replays thinking on the next request to the SAME
        /// provider: Anthropic's signature, or OpenAI Responses' `encrypted_content`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        signature: Option<String>,
        /// Anthropic redacted thinking payload (replayed verbatim, never shown).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        redacted: Option<String>,
    },
    /// A tool the model asked to run.
    ToolCall(ToolCall),
}

/// A tool call the model asked for.
///
/// The loop answers each one with exactly one [`ToolResultMessage`] carrying
/// the same `id`, even when the turn is aborted.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolCall {
    /// The provider's id for this call; [`ToolResultMessage::tool_call_id`] repeats it.
    pub id: String,
    /// The tool's name as the model wrote it; it may name no tool at all.
    pub name: String,
    /// The arguments. Arguments that were not valid JSON arrive as their raw
    /// text under the key `_unparsed`, and the loop answers the call with an
    /// error instead of running the tool.
    pub input: Map<String, Value>,
}

/// Token counts and cost of one model call, or a sum of several.
///
/// The four token counts do not overlap: `input` excludes what was read from
/// or written to the prompt cache. [`Usage::context_tokens`] is their sum.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Usage {
    /// Input tokens billed at the full rate: neither read from nor written to the cache.
    pub input: u64,
    /// Tokens the model generated, reasoning included.
    pub output: u64,
    /// Input tokens served from the provider's prompt cache.
    pub cache_read: u64,
    /// Input tokens written to the provider's prompt cache.
    pub cache_write: u64,
    /// Of `output`, the tokens spent reasoning, when the provider reports them.
    pub reasoning: u64,
    /// When the provider reports it (OpenRouter does), or from a configured price.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_usd: Option<f64>,
}

impl Usage {
    /// Add `u` to this sum. A cost is added when `u` has one, so the sum's
    /// `cost_usd` stays `None` until some call reported a price.
    pub fn add(&mut self, u: &Usage) {
        self.input += u.input;
        self.output += u.output;
        self.cache_read += u.cache_read;
        self.cache_write += u.cache_write;
        self.reasoning += u.reasoning;
        if let Some(c) = u.cost_usd {
            self.cost_usd = Some(self.cost_usd.unwrap_or(0.0) + c);
        }
    }

    /// Tokens the last request actually put in the context window.
    pub fn context_tokens(&self) -> u64 {
        self.input + self.cache_read + self.cache_write + self.output
    }
}

/// Why a model call ended ([`AssistantMessage::stop_reason`]), or a whole
/// turn ([`TurnEnd::stop_reason`](crate::TurnEnd::stop_reason)).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StopReason {
    /// The model finished its answer.
    EndTurn,
    /// The model stopped so the tools it called could run.
    ToolUse,
    /// The answer hit its output token limit. For a turn: a turn cap or the
    /// conversation budget stopped it, and the turn's `error` says which.
    MaxTokens,
    /// The model or the provider's filter declined to answer.
    Refusal,
    /// Cancelled: [`AgentControl::abort`](crate::AgentControl::abort), or the
    /// request's cancel token.
    Aborted,
    /// The call failed, or never went out; the message's `error_message` (or
    /// the turn's `error`) says why.
    Error,
}

crate::word_enum! {
    /// What sort of media a [`MediaPart`] is. The words are OpenRouter's
    /// modality names (`file` = a PDF), one for one with a catalog row's
    /// [`InputModality`](crate::catalog::InputModality). [`MediaKind::as_str`]
    /// is also the word a model or a human reads for the kind.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
    pub enum MediaKind ("media kind") {
        /// A picture.
        Image = "image",
        /// A document; in practice a PDF.
        File = "file",
        /// A sound recording.
        Audio = "audio",
        /// A video clip.
        Video = "video",
    }
}

/// Non-text input riding a user turn or a tool result. Bytes travel inline as
/// base64: every request re-sends history, and the session log must replay
/// without the original file.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MediaPart {
    /// What sort of media it is; a model that does not take this kind gets a
    /// note in its place.
    pub kind: MediaKind,
    /// The MIME type, e.g. `image/png`.
    pub media_type: String,
    /// Base64, no `data:` prefix.
    pub data: String,
    /// Shown to the model when the part is dropped, and used as a filename.
    pub name: String,
}

/// Input to the model in the user's seat: what the human typed, and also what
/// the loop sends on their behalf (steering, the turn guard's nudge, a
/// compaction summary).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UserMessage {
    /// The text, always: media is additive, so every text-only reader stays right.
    pub content: String,
    /// Media attached to the turn ([`AgentHooks::attach_media`](crate::AgentHooks::attach_media)).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub media: Vec<MediaPart>,
}

/// Web searches the PROVIDER ran inside a call (a server tool, not a haest tool call).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct WebSearchUse {
    /// How many searches ran.
    pub searches: u32,
    /// The pages the answer cited, each once, in first-cited order.
    pub sources: Vec<WebSource>,
}

/// One page a provider-side web search cited.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WebSource {
    /// The page's URL.
    pub url: String,
    /// The page's title, when the provider sent one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
}

/// One model call's complete answer, as the [`Provider`](crate::Provider)
/// folded its stream into it.
///
/// A call that failed is an answer too: `stop_reason` [`StopReason::Error`]
/// and the reason in `error_message`, so every failure has one code path.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AssistantMessage {
    /// The answer's blocks, in stream order.
    pub content: Vec<AssistantPart>,
    /// The id of the provider that answered ([`Provider::id`](crate::Provider::id)).
    pub provider: String,
    /// The provider's model id, not the `provider:model` spec.
    pub model: String,
    /// What the call cost.
    pub usage: Usage,
    /// Why the call ended.
    pub stop_reason: StopReason,
    /// Why the call failed, when `stop_reason` is [`StopReason::Error`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_message: Option<String>,
    /// Opaque provider state that must round-trip on the next request to the
    /// SAME provider (e.g. OpenRouter `reasoning_details`). Dropped for any other.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_data: Option<Value>,
    /// The web searches the provider ran during the call; None when there were none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub web_search: Option<WebSearchUse>,
}

impl AssistantMessage {
    /// The empty message every adapter folds its stream into: no content, no
    /// usage, `stop_reason` [`StopReason::EndTurn`].
    pub fn start(provider: &str, model: &str) -> Self {
        Self {
            content: Vec::new(),
            provider: provider.to_string(),
            model: model.to_string(),
            usage: Usage::default(),
            stop_reason: StopReason::EndTurn,
            error_message: None,
            provider_data: None,
            web_search: None,
        }
    }

    /// The tool calls in this answer, in the order the model made them.
    pub fn tool_calls(&self) -> impl Iterator<Item = &ToolCall> {
        self.content.iter().filter_map(|p| match p {
            AssistantPart::ToolCall(c) => Some(c),
            _ => None,
        })
    }

    /// All text parts joined with nothing between them; thinking and tool
    /// calls are left out.
    pub fn text(&self) -> String {
        self.content
            .iter()
            .filter_map(|p| match p {
                AssistantPart::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("")
    }
}

/// The answer to one [`ToolCall`], as history keeps it and the model reads it.
///
/// Built from a tool's [`ToolOutput`](crate::ToolOutput); the host-only parts
/// of that output (`result`, `title`, costs) are not kept here.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolResultMessage {
    /// The [`ToolCall::id`] this answers.
    pub tool_call_id: String,
    /// The [`ToolCall::name`] this answers.
    pub tool_name: String,
    /// What the model reads.
    pub content: String,
    /// The call failed; `content` says why.
    pub is_error: bool,
    /// Images or documents the model should see, not read about.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub media: Vec<MediaPart>,
}

/// One entry of a conversation's history.
///
/// Serialized with a `role` tag: `"user"`, `"assistant"` or `"tool"`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "role", rename_all = "lowercase")]
pub enum Message {
    /// Input in the user's seat.
    User(UserMessage),
    /// A model call's answer.
    Assistant(AssistantMessage),
    /// The answer to one tool call.
    Tool(ToolResultMessage),
}

impl Message {
    /// A user message with `text` and no media.
    pub fn user(text: impl Into<String>) -> Self {
        Message::User(UserMessage {
            content: text.into(),
            media: Vec::new(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_ts_session_message_round_trips() {
        let ts = json!({
            "role": "assistant",
            "content": [
                {"type": "thinking", "thinking": "hm", "signature": "sig"},
                {"type": "text", "text": "hi"},
                {"type": "tool_call", "id": "c1", "name": "Read", "input": {"path": "a.txt"}}
            ],
            "provider": "anthropic",
            "model": "claude-x",
            "usage": {"input": 1, "output": 2, "cacheRead": 3, "cacheWrite": 4, "reasoning": 0, "costUsd": 0.5},
            "stopReason": "tool_use"
        });
        let msg: Message = serde_json::from_value(ts.clone()).unwrap();
        let Message::Assistant(a) = &msg else {
            panic!("not assistant")
        };
        assert_eq!(a.tool_calls().count(), 1);
        assert_eq!(a.stop_reason, StopReason::ToolUse);
        assert_eq!(serde_json::to_value(&msg).unwrap(), ts);
    }

    #[test]
    fn a_ts_user_turn_with_media_round_trips() {
        let ts = json!({
            "role": "user",
            "content": "see these",
            "media": [
                {"kind": "image", "mediaType": "image/png", "data": "AA==", "name": "a.png"},
                {"kind": "file", "mediaType": "application/pdf", "data": "AA==", "name": "b.pdf"},
                {"kind": "audio", "mediaType": "audio/wav", "data": "AA==", "name": "c.wav"},
                {"kind": "video", "mediaType": "video/mp4", "data": "AA==", "name": "d.mp4"}
            ]
        });
        let msg: Message = serde_json::from_value(ts.clone()).unwrap();
        let Message::User(u) = &msg else {
            panic!("not user")
        };
        let kinds: Vec<_> = u.media.iter().map(|p| p.kind).collect();
        assert_eq!(kinds, MediaKind::ALL);
        assert_eq!(serde_json::to_value(&msg).unwrap(), ts);
    }

    #[test]
    fn usage_adds_cost_only_when_reported() {
        let mut u = Usage::default();
        u.add(&Usage {
            input: 5,
            ..Default::default()
        });
        assert_eq!(u.cost_usd, None);
        u.add(&Usage {
            cost_usd: Some(0.25),
            ..Default::default()
        });
        assert_eq!(u.cost_usd, Some(0.25));
        assert_eq!(u.context_tokens(), 5);
    }
}
