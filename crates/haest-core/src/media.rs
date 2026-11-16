//! Media parts ([`MediaPart`]: an image, a PDF, audio, video riding a user
//! turn or a tool result): building one from bytes, and fitting a history to
//! what the current model accepts. Pure, but for [`user_message`], which asks
//! the host.
//!
//! Fitting happens per REQUEST, never on the stored history: a screenshot sent
//! while a text-only model is active is still there, and still seen, after a
//! switch to a vision model. What a model accepts comes from the host's
//! catalog through [`AgentHooks::input_modalities`]; an unknown model gets
//! the history untouched and its provider decides.
//!
//! Who uses what: the loop calls [`user_message`], [`fit_for_model`] and
//! [`messages_for_estimate`]; tools and MCP build parts with
//! [`media_from_bytes`]; provider adapters use [`data_url`] and
//! [`dropped_note`]. A host builds parts with [`media_from_bytes`] in its
//! [`AgentHooks::attach_media`].
//!
//! # Examples
//!
//! ```
//! use std::collections::HashSet;
//!
//! use haest_core::{
//!     Message, UserMessage,
//!     catalog::InputModality,
//!     media::{fit_media, media_from_bytes, media_type_from_name},
//! };
//!
//! let png = [0x89, b'P', b'N', b'G'];
//! let media_type = media_type_from_name("shot.png").unwrap();
//! let part = media_from_bytes(&png, media_type, "shot.png").unwrap();
//! let history = vec![Message::User(UserMessage { content: "What is this?".into(), media: vec![part] })];
//!
//! // A text-only model: the image is replaced by a note the model reads.
//! let text_only: HashSet<_> = [InputModality::Text].into();
//! let fitted = fit_media(&history, Some(&text_only), "example/text-model");
//! let Message::User(user) = &fitted[0] else { unreachable!() };
//! assert!(user.media.is_empty());
//! assert!(user.content.starts_with("What is this?\n\n[image \"shot.png\""));
//!
//! // The stored history is untouched, and an unknown model gets it as is.
//! assert_eq!(fit_media(&history, None, "example/unknown"), history);
//! ```

use std::collections::HashSet;

use base64::{Engine as _, engine::general_purpose::STANDARD};
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use crate::{
    catalog::InputModality,
    deps::AgentHooks,
    messages::{MediaKind, MediaPart, Message, UserMessage},
};

/// The model the GenerateImage tool uses unless told otherwise (the
/// `image_model` setting's default), as an OpenRouter model id.
pub const DEFAULT_IMAGE_MODEL: &str = "google/gemini-nano-banana-2.1";
/// The models a GenerateImage call may pick from by default: the default, one
/// for exact text and transparency, one for edits from several references.
pub const DEFAULT_IMAGE_MODELS: [&str; 3] = [
    DEFAULT_IMAGE_MODEL,
    "openai/gpt-image-2.6-sunburst",
    "black-forest-labs/flux-3-image",
];

/// Every provider haest speaks accepts exactly these four image types.
const IMAGE_TYPES: [&str; 4] = ["image/png", "image/jpeg", "image/gif", "image/webp"];

/// Anthropic refuses an image over 5 MB; the rest are kept sane for history replay.
fn max_bytes(kind: MediaKind) -> usize {
    match kind {
        MediaKind::Image => 5 * 1024 * 1024,
        MediaKind::File | MediaKind::Audio | MediaKind::Video => 20 * 1024 * 1024,
    }
}

/// The kind of media a MIME type is, or None when haest would not send it to
/// a model.
///
/// An image must be PNG, JPEG, GIF or WebP (the four every provider takes);
/// `application/pdf` is a [`MediaKind::File`]; any `audio/*` and `video/*`
/// pass.
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

/// The media type for a file name's extension (any case), or None when haest
/// would not send such a file as media. Knows the image, PDF, audio and video
/// extensions [`media_kind_of`] accepts.
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

/// A media part holding `bytes` (base64-encoded), named `name`.
///
/// # Errors
///
/// The reason these bytes cannot be a part, worded for the model (a tool
/// returns it as is): `media_type` is not one [`media_kind_of`] accepts, or
/// the bytes are over the kind's limit (5 MB for an image, Anthropic's cap;
/// 20 MB for the rest).
pub fn media_from_bytes(bytes: &[u8], media_type: &str, name: &str) -> Result<MediaPart, String> {
    let Some(kind) = media_kind_of(media_type) else {
        return Err(format!(
            "{name} ({media_type}) is not a media type haest can send to a model"
        ));
    };
    if bytes.len() > max_bytes(kind) {
        return Err(format!(
            "{name} is {}; the limit for {} input is {}",
            kb(bytes.len()),
            kind.as_str(),
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

/// What the model reads in place of a part it cannot receive: the part's
/// kind, name, type and size, that `model` does not accept the kind, and to
/// switch models to see it.
pub fn dropped_note(p: &MediaPart, model: &str) -> String {
    let kind = p.kind.as_str();
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

/// The history as THIS model may receive it: every media part of a kind
/// `accepts` lacks is taken out of its user turn or tool result, and a
/// [`dropped_note`] naming `model` is added to that message's text in its
/// place. Assistant messages and messages with nothing to drop come back as
/// they were.
///
/// `accepts` None = the catalog does not know the model, so nothing is
/// touched and the provider decides. The loop calls [`fit_for_model`], which
/// also tells the host.
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

/// [`fit_media`], plus a report of what was withheld: the note tells the MODEL,
/// [`AgentHooks::on_media_withheld`] is how the HUMAN finds out the screenshot
/// never reached it.
///
/// The hook is called once per call with every withheld part, and only when
/// there is one. It runs on every model call, so the same part is reported
/// again each time: the host dedupes.
pub fn fit_for_model(
    messages: &[Message],
    accepts: Option<&HashSet<InputModality>>,
    model: &str,
    hooks: &dyn AgentHooks,
) -> Vec<Message> {
    if let Some(accepts) = accepts {
        let withheld: Vec<MediaPart> = messages
            .iter()
            .flat_map(media_of)
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

/// The media a message carries: a user turn's or a tool result's.
fn media_of(m: &Message) -> &[MediaPart] {
    match m {
        Message::User(u) => &u.media,
        Message::Tool(t) => &t.media,
        Message::Assistant(_) => &[],
    }
}

/// The history for the request-size estimate ([`crate::limits`]): its JSON
/// with every message's media taken out, and the media's own rough token
/// cost.
///
/// Base64 counted as text would read a 1 MB screenshot as ~350K tokens. An
/// image counts a fixed 1600 tokens (about what every provider charges); any
/// other part one token per 64 bytes.
pub fn messages_for_estimate(messages: &[Message]) -> (String, u64) {
    let mut value = serde_json::to_value(messages).unwrap_or(Value::Null);
    if let Value::Array(items) = &mut value {
        for item in items {
            if let Some(obj) = item.as_object_mut() {
                obj.remove("media");
            }
        }
    }
    let tokens = messages.iter().flat_map(media_of).map(media_tokens).sum();
    (value.to_string(), tokens)
}

/// A user turn: the text as typed, plus whatever its attachment links point at.
/// A link that could not be fetched is said in the text, so the model never guesses.
///
/// The media and the notes come from the host's
/// [`AgentHooks::attach_media`] (none by default); each note is added after
/// the text, a blank line apart. `cancel` is handed to the hook, which may
/// fetch.
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

/// The part as a `data:<type>;base64,<data>` URL, for wire formats that take
/// one (Chat Completions, Responses).
pub fn data_url(p: &MediaPart) -> String {
    format!("data:{};base64,{}", p.media_type, p.data)
}

#[cfg(test)]
mod tests;
