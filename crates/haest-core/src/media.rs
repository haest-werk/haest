//! Media parts ([`MediaPart`]: an image, a PDF, audio, video riding a user
//! turn or a tool result): building one from bytes, and fitting a history to
//! what the current model accepts.
//!
//! Fitting happens per REQUEST, never on the stored history: a screenshot sent
//! while a text-only model is active is still there, and still seen, after a
//! switch to a vision model. What a model accepts comes from the host's
//! catalog through [`ModelFacts::input_modalities`](crate::ModelFacts::input_modalities); an unknown model gets
//! the history untouched and its provider decides.
//!
//! Who uses what: tools, MCP and a host's
//! [`AgentHooks::attach_media`] build parts with [`media_from_bytes`]; the
//! loop fits them to each model call; provider adapters write what their
//! wire format cannot carry as a note ([`dropped_note`]) or a `data:` URL
//! ([`data_url`]).
//!
//! # Examples
//!
//! ```
//! use haest_core::media::{MediaError, media_from_bytes, media_type_from_name};
//!
//! let png = [0x89, b'P', b'N', b'G'];
//! let media_type = media_type_from_name("shot.png").unwrap();
//! let part = media_from_bytes(&png, media_type, "shot.png").unwrap();
//! assert_eq!(part.media_type, "image/png");
//!
//! // Too big for its kind: refused with a reason, before anything is sent.
//! let huge = vec![0u8; 6 * 1024 * 1024];
//! assert!(matches!(
//!     media_from_bytes(&huge, "image/png", "huge.png"),
//!     Err(MediaError::TooBig { .. })
//! ));
//! ```

mod fit;
#[cfg(test)]
mod tests;
mod wire;

use std::path::Path;

use base64::{Engine as _, engine::general_purpose::STANDARD};
use serde_json::Value;
use tokio_util::sync::CancellationToken;

pub(crate) use fit::fit_for_model;
pub use wire::{data_url, dropped_note};

use crate::{
    deps::AgentHooks,
    messages::{MediaKind, MediaPart, Message, UserMessage},
};

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
/// `application/pdf` (exactly) is a [`MediaKind::File`]; any `audio/*` and
/// `video/*` pass.
pub(crate) fn media_kind_of(media_type: &str) -> Option<MediaKind> {
    let kind = if media_type.starts_with("image/") {
        MediaKind::Image
    } else if media_type == "application/pdf" {
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
/// extensions haest sends as media. `name` may be a path; a name with no
/// extension (`png`) is None.
pub fn media_type_from_name(name: &str) -> Option<&'static str> {
    let ext = Path::new(name).extension()?.to_str()?.to_ascii_lowercase();
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

/// Why bytes cannot be a media part: what [`media_from_bytes`] refuses. Its
/// text is worded for the model, so a tool returns it as is.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum MediaError {
    /// The media type is not one haest sends to a model.
    #[error("{name} ({media_type}) is not a media type haest can send to a model")]
    NotMedia {
        /// The part's name.
        name: String,
        /// Its media type, as given.
        media_type: String,
    },
    /// Over the limit for its kind.
    #[error("{name} is {}; the limit for {} input is {}", kb(*size), kind.as_str(), kb(*limit))]
    TooBig {
        /// The part's name.
        name: String,
        /// Its kind.
        kind: MediaKind,
        /// Its size in bytes.
        size: usize,
        /// The kind's limit in bytes.
        limit: usize,
    },
}

/// A media part holding `bytes` (base64-encoded), named `name`.
///
/// # Errors
///
/// [`MediaError::NotMedia`] when `media_type` is not one haest sends as
/// media, [`MediaError::TooBig`] when the bytes are over the kind's limit
/// (5 MB for an image, Anthropic's cap; 20 MB for the rest).
pub fn media_from_bytes(
    bytes: &[u8],
    media_type: &str,
    name: &str,
) -> Result<MediaPart, MediaError> {
    let Some(kind) = media_kind_of(media_type) else {
        return Err(MediaError::NotMedia {
            name: name.into(),
            media_type: media_type.into(),
        });
    };
    if bytes.len() > max_bytes(kind) {
        return Err(MediaError::TooBig {
            name: name.into(),
            kind,
            size: bytes.len(),
            limit: max_bytes(kind),
        });
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

/// The history for the request-size estimate ([`crate::caps`]): its JSON
/// with every message's media taken out, and the media's own rough token
/// cost.
///
/// Base64 counted as text would read a 1 MB screenshot as ~350K tokens. An
/// image counts a fixed 1600 tokens (about what every provider charges); any
/// other part one token per 64 bytes.
pub(crate) fn messages_for_estimate(messages: &[Message]) -> (String, u64) {
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
pub(crate) async fn user_message(
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
