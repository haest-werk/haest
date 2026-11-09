//! Media parts in the two OpenAI-shaped wire formats. Pure.
//!
//! Chat Completions (OpenRouter and compatibles): images as `image_url`, PDFs as
//! `file`, audio as `input_audio`, video as `video_url` -- OpenRouter routes each
//! to the upstream's native shape. Responses (OpenAI direct): `input_image` and
//! `input_file`; it takes no audio or video, so those become a note.
//!
//! Neither format lets a tool result carry media, so a tool's media rides a user
//! message placed after the run of tool results.

use kern_core::{
    media::{data_url, dropped_note},
    messages::{MediaKind, MediaPart},
};
use serde_json::{Value, json};

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
    }
}

/// A user message's content: the plain string when there is no media (the common case, unchanged on the wire).
pub fn user_content(text: &str, media: &[MediaPart]) -> Value {
    if media.is_empty() {
        return Value::String(text.to_string());
    }
    let lead = (!text.is_empty()).then(|| json!({"type": "text", "text": text}));
    Value::Array(
        lead.into_iter()
            .chain(media.iter().map(chat_part))
            .collect(),
    )
}

/// Tool name + the media its result returned, in history order.
pub type ToolMedia = Vec<(String, Vec<MediaPart>)>;

/// The lead line of the user message that carries tool media, in both formats.
pub fn tool_media_lead(results: &ToolMedia) -> String {
    let names = results
        .iter()
        .map(|(tool, _)| tool.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    let plural = if results.len() == 1 { "" } else { "s" };
    format!("Media returned by the {names} result{plural} above:")
}

fn all_media(results: &ToolMedia) -> Vec<MediaPart> {
    results
        .iter()
        .flat_map(|(_, m)| m.iter().cloned())
        .collect()
}

pub fn tool_media_message(results: &ToolMedia) -> Value {
    json!({"role": "user", "content": user_content(&tool_media_lead(results), &all_media(results))})
}

fn responses_part(p: &MediaPart) -> Value {
    match p.kind {
        MediaKind::Image => {
            json!({"type": "input_image", "image_url": data_url(p), "detail": "auto"})
        }
        MediaKind::File => {
            json!({"type": "input_file", "filename": p.name, "file_data": data_url(p)})
        }
        MediaKind::Audio | MediaKind::Video => json!({
            "type": "input_text",
            "text": dropped_note(p, "the OpenAI Responses API"),
        }),
    }
}

/// A Responses user item's content: the plain string when there is no media.
pub fn responses_user_content(text: &str, media: &[MediaPart]) -> Value {
    if media.is_empty() {
        return Value::String(text.to_string());
    }
    let lead = (!text.is_empty()).then(|| json!({"type": "input_text", "text": text}));
    Value::Array(
        lead.into_iter()
            .chain(media.iter().map(responses_part))
            .collect(),
    )
}

/// The Responses user item that carries a run's tool media.
pub fn responses_tool_media_item(results: &ToolMedia) -> Value {
    json!({
        "role": "user",
        "content": responses_user_content(&tool_media_lead(results), &all_media(results)),
    })
}
