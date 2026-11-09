//! Media parts: building one from bytes, and fitting a history to what the
//! current model accepts. Pure.
//!
//! Fitting happens per REQUEST, never on the stored history: a screenshot sent
//! while a text-only model is active is still there, and still seen, after a
//! switch to a vision model.

use std::collections::HashSet;

use base64::{Engine as _, engine::general_purpose::STANDARD};
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use crate::{
    catalog::InputModality,
    deps::AgentHooks,
    messages::{MediaKind, MediaPart, Message, UserMessage},
};

/// Every provider kern speaks accepts exactly these four image types.
const IMAGE_TYPES: [&str; 4] = ["image/png", "image/jpeg", "image/gif", "image/webp"];

/// Anthropic refuses an image over 5 MB; the rest are kept sane for history replay.
fn max_bytes(kind: MediaKind) -> usize {
    match kind {
        MediaKind::Image => 5 * 1024 * 1024,
        MediaKind::File | MediaKind::Audio | MediaKind::Video => 20 * 1024 * 1024,
    }
}

pub fn media_kind_of(media_type: &str) -> Option<MediaKind> {
    let kind = if media_type.starts_with("image/") {
        MediaKind::Image
    } else if media_type.starts_with("application/pdf") {
        MediaKind::File
    } else if media_type.starts_with("audio/") {
        MediaKind::Audio
    } else if media_type.starts_with("video/") {
        MediaKind::Video
    } else {
        return None;
    };
    if kind == MediaKind::Image && !IMAGE_TYPES.contains(&media_type) {
        return None;
    }
    Some(kind)
}

/// Media type from a file name, or None when kern would not send it as media.
pub fn media_type_from_name(name: &str) -> Option<&'static str> {
    let ext = name.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
    Some(match ext.as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "pdf" => "application/pdf",
        "mp3" => "audio/mpeg",
        "wav" => "audio/wav",
        "m4a" => "audio/mp4",
        "ogg" => "audio/ogg",
        "mp4" => "video/mp4",
        "mov" => "video/quicktime",
        "webm" => "video/webm",
        _ => return None,
    })
}

fn kb(bytes: usize) -> String {
    format!("{} KB", ((bytes as f64 / 1024.0).round() as usize).max(1))
}

fn kind_name(kind: MediaKind) -> &'static str {
    match kind {
        MediaKind::Image => "image",
        MediaKind::File => "file",
        MediaKind::Audio => "audio",
        MediaKind::Video => "video",
    }
}

/// A media part, or the reason these bytes cannot be one (said to the model verbatim).
pub fn media_from_bytes(bytes: &[u8], media_type: &str, name: &str) -> Result<MediaPart, String> {
    let Some(kind) = media_kind_of(media_type) else {
        return Err(format!(
            "{name} ({media_type}) is not a media type kern can send to a model"
        ));
    };
    if bytes.len() > max_bytes(kind) {
        return Err(format!(
            "{name} is {}; the limit for {} input is {}",
            kb(bytes.len()),
            kind_name(kind),
            kb(max_bytes(kind))
        ));
    }
    Ok(MediaPart {
        kind,
        media_type: media_type.to_string(),
        data: STANDARD.encode(bytes),
        name: name.to_string(),
    })
}

fn raw_bytes(p: &MediaPart) -> usize {
    p.data.len() * 3 / 4
}

fn modality(kind: MediaKind) -> InputModality {
    match kind {
        MediaKind::Image => InputModality::Image,
        MediaKind::File => InputModality::File,
        MediaKind::Audio => InputModality::Audio,
        MediaKind::Video => InputModality::Video,
    }
}

/// What the model reads in place of a part it cannot receive.
pub fn dropped_note(p: &MediaPart, model: &str) -> String {
    let kind = kind_name(p.kind);
    format!(
        "[{kind} \"{}\" ({}, {}) not shown: {model} does not accept {kind} input. Switch to a model that does to see it.]",
        p.name,
        p.media_type,
        kb(raw_bytes(p))
    )
}

fn fit_one(
    content: &str,
    media: &[MediaPart],
    accepts: &HashSet<InputModality>,
    model: &str,
) -> Option<(String, Vec<MediaPart>)> {
    if media.iter().all(|p| accepts.contains(&modality(p.kind))) {
        return None;
    }
    let (kept, dropped): (Vec<_>, Vec<_>) = media
        .iter()
        .cloned()
        .partition(|p| accepts.contains(&modality(p.kind)));
    let notes = dropped.iter().map(|p| dropped_note(p, model));
    let content = std::iter::once(content.to_string())
        .chain(notes)
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n");
    Some((content, kept))
}

/// The history as THIS model may receive it. `accepts` None = the catalog does
/// not know the model, so nothing is touched and the provider decides.
pub fn fit_media(
    messages: &[Message],
    accepts: Option<&HashSet<InputModality>>,
    model: &str,
) -> Vec<Message> {
    let Some(accepts) = accepts else {
        return messages.to_vec();
    };
    messages
        .iter()
        .map(|m| match m {
            Message::User(u) => match fit_one(&u.content, &u.media, accepts, model) {
                Some((content, media)) => Message::User(UserMessage { content, media }),
                None => m.clone(),
            },
            Message::Tool(t) => match fit_one(&t.content, &t.media, accepts, model) {
                Some((content, media)) => {
                    let mut t = t.clone();
                    t.content = content;
                    t.media = media;
                    Message::Tool(t)
                }
                None => m.clone(),
            },
            Message::Assistant(_) => m.clone(),
        })
        .collect()
}

/// `fit_media`, plus a report of what was withheld: the note tells the MODEL,
/// the hook is how the HUMAN finds out the screenshot never reached it.
pub fn fit_for_model(
    messages: &[Message],
    accepts: Option<&HashSet<InputModality>>,
    model: &str,
    hooks: &dyn AgentHooks,
) -> Vec<Message> {
    if let Some(accepts) = accepts {
        let withheld: Vec<MediaPart> = messages
            .iter()
            .flat_map(|m| match m {
                Message::User(u) => u.media.as_slice(),
                Message::Tool(t) => t.media.as_slice(),
                Message::Assistant(_) => &[],
            })
            .filter(|p| !accepts.contains(&modality(p.kind)))
            .cloned()
            .collect();
        if !withheld.is_empty() {
            hooks.on_media_withheld(&withheld, model);
        }
    }
    fit_media(messages, accepts, model)
}

/// Rough token cost of one part. An image is a fixed ~1.6K tokens on every provider; the rest scale with size.
fn media_tokens(p: &MediaPart) -> u64 {
    match p.kind {
        MediaKind::Image => 1600,
        _ => raw_bytes(p).div_ceil(64) as u64,
    }
}

/// JSON of the history with media bytes replaced by their token cost, for the request-size estimate.
pub fn messages_for_estimate(messages: &[Message]) -> (String, u64) {
    let mut tokens = 0;
    let mut value = serde_json::to_value(messages).unwrap_or(Value::Null);
    if let Value::Array(items) = &mut value {
        for item in items {
            if let Some(obj) = item.as_object_mut() {
                obj.remove("media");
            }
        }
    }
    for m in messages {
        let media = match m {
            Message::User(u) => u.media.as_slice(),
            Message::Tool(t) => t.media.as_slice(),
            Message::Assistant(_) => &[],
        };
        tokens += media.iter().map(media_tokens).sum::<u64>();
    }
    (value.to_string(), tokens)
}

/// A user turn: the text as typed, plus whatever its attachment links point at.
/// A link that could not be fetched is said in the text, so the model never guesses.
pub async fn user_message(
    text: &str,
    hooks: &dyn AgentHooks,
    cancel: &CancellationToken,
) -> UserMessage {
    let (media, notes) = hooks.attach_media(text, cancel).await;
    let content = std::iter::once(text.to_string())
        .chain(notes)
        .collect::<Vec<_>>()
        .join("\n\n");
    UserMessage { content, media }
}

/// `data:` URL for wire formats that take one (Chat Completions, Responses).
pub fn data_url(p: &MediaPart) -> String {
    format!("data:{};base64,{}", p.media_type, p.data)
}

#[cfg(test)]
mod tests;
