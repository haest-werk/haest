//! Provider-neutral conversation model. This is what the loop keeps in memory,
//! what the session JSONL stores, and what each provider adapter converts to
//! its own wire format. Nothing here knows about a model vendor or a host.
//!
//! The serde shape is the TypeScript kern's, field for field (camelCase keys,
//! `role` / `type` tags), so a session file written by either kern replays in
//! the other.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// One block of an assistant message.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AssistantPart {
    Text {
        text: String,
    },
    Thinking {
        thinking: String,
        /// Opaque token that replays thinking on the next request to the SAME
        /// provider: Anthropic's signature, or OpenAI Responses' `encrypted_content`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        signature: Option<String>,
        /// Anthropic redacted thinking payload (replayed verbatim, never shown).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        redacted: Option<String>,
    },
    ToolCall(ToolCall),
}

/// A tool call the model asked for.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub input: Map<String, Value>,
}

/// Token counts of one model call (or a sum of them).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Usage {
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_write: u64,
    pub reasoning: u64,
    /// When the provider reports it (OpenRouter does), or from a configured price.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_usd: Option<f64>,
}

impl Usage {
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StopReason {
    EndTurn,
    ToolUse,
    MaxTokens,
    Refusal,
    Aborted,
    Error,
}

crate::word_enum! {
    /// OpenRouter's modality names (`file` = a PDF), so gating against a model's
    /// catalog row is a set lookup. [`MediaKind::as_str`] is also the word a
    /// model or a human reads for the kind.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
    pub enum MediaKind ("media kind") {
        Image = "image",
        File = "file",
        Audio = "audio",
        Video = "video",
    }
}

/// Non-text input riding a user turn or a tool result. Bytes travel inline as
/// base64: every request re-sends history, and the session log must replay
/// without the original file.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MediaPart {
    pub kind: MediaKind,
    pub media_type: String,
    /// Base64, no `data:` prefix.
    pub data: String,
    /// Shown to the model when the part is dropped, and used as a filename.
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UserMessage {
    /// The text, always: media is additive, so every text-only reader stays right.
    pub content: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub media: Vec<MediaPart>,
}

/// Web searches the PROVIDER ran inside a call (a server tool, not a kern tool call).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct WebSearchUse {
    pub searches: u32,
    pub sources: Vec<WebSource>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WebSource {
    pub url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AssistantMessage {
    pub content: Vec<AssistantPart>,
    pub provider: String,
    pub model: String,
    pub usage: Usage,
    pub stop_reason: StopReason,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_message: Option<String>,
    /// Opaque provider state that must round-trip on the next request to the
    /// SAME provider (e.g. OpenRouter `reasoning_details`). Dropped for any other.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_data: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub web_search: Option<WebSearchUse>,
}

impl AssistantMessage {
    /// The empty message every adapter folds its stream into.
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

    pub fn tool_calls(&self) -> impl Iterator<Item = &ToolCall> {
        self.content.iter().filter_map(|p| match p {
            AssistantPart::ToolCall(c) => Some(c),
            _ => None,
        })
    }

    /// All text parts, joined.
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

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolResultMessage {
    pub tool_call_id: String,
    pub tool_name: String,
    pub content: String,
    pub is_error: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub media: Vec<MediaPart>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "role", rename_all = "lowercase")]
pub enum Message {
    User(UserMessage),
    Assistant(AssistantMessage),
    Tool(ToolResultMessage),
}

impl Message {
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
