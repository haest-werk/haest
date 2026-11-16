//! How a provider adapter writes media its wire format cannot carry as a
//! part: as a note the model reads, or as a `data:` URL.

use super::{kb, raw_bytes};
use crate::messages::MediaPart;

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

/// The part as a `data:<type>;base64,<data>` URL, for wire formats that take
/// one (Chat Completions, Responses).
pub fn data_url(p: &MediaPart) -> String {
    format!("data:{};base64,{}", p.media_type, p.data)
}
