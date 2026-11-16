//! Fitting a history to what one model accepts: every part of a kind it
//! cannot take is withheld from that request, and a note stands in for it.

use std::collections::HashSet;

use super::{dropped_note, media_of};
use crate::{
    catalog::InputModality,
    deps::AgentHooks,
    messages::{MediaKind, MediaPart, Message, UserMessage},
};

fn modality(kind: MediaKind) -> InputModality {
    match kind {
        MediaKind::Image => InputModality::Image,
        MediaKind::File => InputModality::File,
        MediaKind::Audio => InputModality::Audio,
        MediaKind::Video => InputModality::Video,
    }
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
pub(super) fn fit_media(
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
pub(crate) fn fit_for_model(
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
