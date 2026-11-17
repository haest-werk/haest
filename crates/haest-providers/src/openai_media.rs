//! Media parts in the two OpenAI-shaped wire formats. Pure.
//!
//! Chat Completions (OpenRouter and compatibles): images as `image_url`, PDFs as
//! `file`, audio as `input_audio`, video as `video_url` -- OpenRouter routes each
//! to the upstream's native shape. Responses (OpenAI direct): `input_image` and
//! `input_file`; it takes no audio or video, so those become a note.
//!
//! Neither format lets a tool result carry media, so a tool's media rides a user
//! message placed after the run of tool results ([`with_tool_media`]).

use haest_core::{
    media::{data_url, dropped_note},
    messages::{MediaKind, MediaPart, Message},
};
use serde_json::{Value, json};

/// One format's content parts: what its text part is called, and its media parts.
pub(crate) struct Parts {
    text: &'static str,
    media: fn(&MediaPart) -> Value,
}

/// Chat Completions.
pub(crate) const CHAT: Parts = Parts {
    text: "text",
    media: chat_part,
};

/// The Responses API.
pub(crate) const RESPONSES: Parts = Parts {
    text: "input_text",
    media: responses_part,
};

/// OpenRouter's `input_audio.format` is the subtype, except mpeg, which it calls mp3.
fn audio_format(media_type: &str) -> &str {
    match media_type.split('/').nth(1).unwrap_or("wav") {
        "mpeg" => "mp3",
        sub => sub,
    }
}

fn chat_part(p: &MediaPart) -> Value {
    match p.kind {
        MediaKind::Image => json!({"type": "image_url", "image_url": {"url": data_url(p)}}),
        MediaKind::File => {
            json!({"type": "file", "file": {"filename": p.name, "file_data": data_url(p)}})
        }
        MediaKind::Audio => json!({
            "type": "input_audio",
            "input_audio": {"data": p.data, "format": audio_format(&p.media_type)},
        }),
        MediaKind::Video => json!({"type": "video_url", "video_url": {"url": data_url(p)}}),
        _ => json!({"type": "text", "text": dropped_note(p, "the chat completions API")}),
    }
}

fn responses_part(p: &MediaPart) -> Value {
    match p.kind {
        MediaKind::Image => {
            json!({"type": "input_image", "image_url": data_url(p), "detail": "auto"})
        }
        MediaKind::File => {
            json!({"type": "input_file", "filename": p.name, "file_data": data_url(p)})
        }
        // Audio, video, and any kind added later.
        _ => json!({
            "type": "input_text",
            "text": dropped_note(p, "the OpenAI Responses API"),
        }),
    }
}

impl Parts {
    /// A user message's content: the plain string when there is no media (the common case, unchanged on the wire).
    pub(crate) fn user_content(&self, text: &str, media: &[MediaPart]) -> Value {
        if media.is_empty() {
            return Value::String(text.to_string());
        }
        let lead = (!text.is_empty()).then(|| json!({"type": self.text, "text": text}));
        Value::Array(
            lead.into_iter()
                .chain(media.iter().map(self.media))
                .collect(),
        )
    }
}

/// Tool name + the media its result returned, in history order.
type ToolMedia = Vec<(String, Vec<MediaPart>)>;

/// The user message that carries a run's tool media; none when the run had none.
fn flush(held: &mut ToolMedia, parts: &Parts, out: &mut Vec<Value>) {
    if held.is_empty() {
        return;
    }
    let names: Vec<&str> = held.iter().map(|(tool, _)| tool.as_str()).collect();
    let plural = if held.len() == 1 { "" } else { "s" };
    let lead = format!(
        "Media returned by the {} result{plural} above:",
        names.join(", ")
    );
    let media: Vec<MediaPart> = held.drain(..).flat_map(|(_, m)| m).collect();
    out.push(json!({"role": "user", "content": parts.user_content(&lead, &media)}));
}

/// Every message through `each`, in order, and each run of tool results'
/// media as one user message right AFTER the run: one between two results
/// orphans the rest.
pub(crate) fn with_tool_media(
    messages: &[Message],
    parts: &Parts,
    out: &mut Vec<Value>,
    mut each: impl FnMut(&Message, &mut Vec<Value>),
) {
    let mut held: ToolMedia = Vec::new();
    for m in messages {
        if !matches!(m, Message::Tool(_)) {
            flush(&mut held, parts, out);
        }
        each(m, out);
        if let Message::Tool(t) = m
            && !t.media.is_empty()
        {
            held.push((t.tool_name.clone(), t.media.clone()));
        }
    }
    flush(&mut held, parts, out);
}
