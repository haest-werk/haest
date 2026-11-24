//! A client's prompt as one turn of the conversation: its content as the
//! human's text, a slash command the conversation answers run first, the
//! turn's end as the prompt's stop reason.

use std::sync::Arc;

use agent_client_protocol::{
    Error, ErrorCode, RequestCancellation,
    v1::schema::{ContentBlock, EmbeddedResourceResource, PromptResponse, StopReason as AcpStop},
};
use haest_core::error::report;
use haest_mods::CommandOutcome;
use haest_session::{Control, Input, SessionError};
use tokio_util::sync::CancellationToken;

use crate::{open::COMPACT, state::Live, translate::stop_reason};

/// The prompt's blocks as the human's text: text as it is, a link to a
/// resource as its path (its URI when it is no file), and an embedded text
/// resource after the text, in a `<context>` block that names it.
///
/// # Errors
///
/// An image, audio or binary resource: haest-acp says it takes none of them.
pub(crate) fn text_of(blocks: &[ContentBlock]) -> Result<String, Error> {
    let mut text = String::new();
    let mut context = String::new();
    for block in blocks {
        match block {
            ContentBlock::Text(t) => text.push_str(&t.text),
            ContentBlock::ResourceLink(link) => {
                text.push_str(link.uri.strip_prefix("file://").unwrap_or(&link.uri));
            }
            ContentBlock::Resource(embedded) => match &embedded.resource {
                EmbeddedResourceResource::TextResourceContents(r) => {
                    context.push_str(&format!(
                        "\n\n<context ref=\"{}\">\n{}\n</context>",
                        r.uri, r.text
                    ));
                }
                _ => return Err(not_taken("a binary resource")),
            },
            ContentBlock::Image(_) => return Err(not_taken("an image")),
            ContentBlock::Audio(_) => return Err(not_taken("audio")),
            _ => return Err(not_taken("a kind of content")),
        }
    }
    text.push_str(&context);
    Ok(text)
}

fn not_taken(what: &str) -> Error {
    Error::invalid_params().data(format!(
        "haest takes text, resource links and text resources in a prompt, not {what}"
    ))
}

/// What a line comes to: text for the model, or nothing (a command did its
/// work).
async fn resolve(live: &Live, text: String) -> Result<Option<String>, Error> {
    let Some(rest) = text.trim().strip_prefix('/') else {
        return Ok(Some(text));
    };
    let (name, args) = rest
        .split_once(char::is_whitespace)
        .map_or((rest, ""), |(n, a)| (n, a.trim()));
    let (name, args) = (name.to_string(), args.to_string());
    if name == COMPACT {
        let cancel = CancellationToken::new();
        // A compaction that fails says why in the stream.
        let _ = live
            .session
            .with(move |rt| Box::pin(async move { rt.compact(&cancel).await }))
            .await
            .map_err(session_failed)?;
        return Ok(None);
    }
    let outcome = live
        .session
        .with(move |rt| Box::pin(async move { rt.command(&name, &args).await }))
        .await
        .map_err(session_failed)?;
    Ok(match outcome {
        CommandOutcome::Unknown => Some(text),
        CommandOutcome::Prompt(text) => Some(text),
        // Handled, or an outcome newer than this crate: the command had its say.
        _ => None,
    })
}

/// Run the prompt `blocks` as a turn of `live`'s conversation, and say how
/// it ended. The client cancelling the request stops the turn, as
/// `session/cancel` does. The answer goes out once the client has been told
/// every update of the turn.
pub(crate) async fn run(
    live: Arc<Live>,
    blocks: &[ContentBlock],
    cancelled: RequestCancellation,
) -> Result<PromptResponse, Error> {
    let text = text_of(blocks)?;
    let Some(text) = resolve(&live, text).await? else {
        live.told().await;
        return Ok(PromptResponse::new(AcpStop::EndTurn));
    };
    let turn = live
        .session
        .send_input(Input::human(text))
        .map_err(session_failed)?;
    tokio::pin!(turn);
    let ended = tokio::select! {
        ended = &mut turn => ended,
        () = cancelled.cancelled() => {
            let _ = live.session.control(Control::Interrupt).await;
            turn.await
        }
    };
    let outcome = ended.map_err(session_failed)?;
    live.told().await;
    match stop_reason(outcome.stop_reason, outcome.limit.as_ref()) {
        Some(reason) => Ok(PromptResponse::new(reason)),
        None => {
            let why = outcome
                .error
                .unwrap_or_else(|| "the turn failed".to_string());
            Err(Error::new(ErrorCode::INTERNAL_ERROR, why))
        }
    }
}

/// The session could not take the request.
pub(crate) fn session_failed(e: SessionError) -> Error {
    Error::new(ErrorCode::INTERNAL_ERROR, report(&e))
}

#[cfg(test)]
mod tests {
    use agent_client_protocol::v1::schema::{
        EmbeddedResource, ImageContent, ResourceLink, TextContent, TextResourceContents,
    };

    use super::*;

    #[test]
    fn a_prompt_is_its_text_its_links_paths_and_its_resources_after() {
        let blocks = vec![
            ContentBlock::Text(TextContent::new("look at ")),
            ContentBlock::ResourceLink(ResourceLink::new("a.rs", "file:///w/a.rs")),
            ContentBlock::Text(TextContent::new(" please")),
            ContentBlock::Resource(EmbeddedResource::new(
                EmbeddedResourceResource::TextResourceContents(TextResourceContents::new(
                    "fn main() {}",
                    "file:///w/b.rs",
                )),
            )),
        ];
        let text = text_of(&blocks).unwrap();
        let (said, context) = text.split_once("\n\n").unwrap();
        assert_eq!(said, "look at /w/a.rs please");
        assert!(context.contains("file:///w/b.rs") && context.contains("fn main() {}"));
    }

    /// haest never says it takes images: one is refused, never dropped.
    #[test]
    fn an_image_is_refused_not_dropped() {
        let blocks = vec![
            ContentBlock::Text(TextContent::new("what is this")),
            ContentBlock::Image(ImageContent::new("aGk=", "image/png")),
        ];
        assert!(text_of(&blocks).is_err());
    }
}
