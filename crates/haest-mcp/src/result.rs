//! An MCP `tools/call` result as a haest [`ToolOutput`].

use base64::{
    Engine as _,
    alphabet::STANDARD,
    engine::{DecodePaddingMode, GeneralPurpose, GeneralPurposeConfig},
};
use haest_core::{media::media_from_bytes, messages::MediaPart, tool::ToolOutput};
use rmcp::model::{CallToolResult, ContentBlock, ResourceContents};

/// Base64 the way a lenient decoder reads it: padding optional, whitespace ignored.
fn decode_base64(data: &str) -> Option<Vec<u8>> {
    let engine = GeneralPurpose::new(
        &STANDARD,
        GeneralPurposeConfig::new().with_decode_padding_mode(DecodePaddingMode::Indifferent),
    );
    let compact: String = data.chars().filter(|c| !c.is_ascii_whitespace()).collect();
    engine.decode(compact).ok()
}

/// An MCP image or audio part (`kind`) becomes media (collected into
/// `media`); the model reads one line in its place.
fn attach(kind: &str, data: &str, mime: &str, index: usize, media: &mut Vec<MediaPart>) -> String {
    if data.is_empty() || mime.is_empty() {
        return format!("[{kind} omitted: no data]");
    }
    let ext = mime.split('/').nth(1).unwrap_or("bin");
    let name = format!("{kind}-{}.{ext}", index + 1);
    let Some(bytes) = decode_base64(data) else {
        return format!("[{kind} omitted: its data is not base64]");
    };
    match media_from_bytes(&bytes, mime, &name) {
        Ok(part) => {
            let line = format!("[{} {} attached]", part.kind.as_str(), part.name);
            media.push(part);
            line
        }
        Err(reason) => format!("[{reason}]"),
    }
}

fn part_text(part: &ContentBlock, index: usize, media: &mut Vec<MediaPart>) -> String {
    match part {
        ContentBlock::Text(t) => t.text.clone(),
        ContentBlock::Image(p) => attach("image", &p.data, &p.mime_type, index, media),
        ContentBlock::Audio(p) => attach("audio", &p.data, &p.mime_type, index, media),
        ContentBlock::Resource(r) => match &r.resource {
            ResourceContents::TextResourceContents { text, .. } => text.clone(),
            ResourceContents::BlobResourceContents { uri, .. } => format!("[resource {uri}]"),
            _ => "[resource omitted]".into(),
        },
        ContentBlock::ResourceLink(link) => format!("[resource_link {} omitted]", link.uri),
        _ => "[a part haest does not know, omitted]".into(),
    }
}

/// What the model reads of an MCP `tools/call` result.
///
/// Every part of `content` becomes one line or more, joined by newlines: a
/// text part its text, a resource its text (or `[resource <uri>]` when it has
/// none). An image or audio part haest can send becomes media on the output
/// ([`ToolOutput::media`]) and a line saying it is attached; one with no data,
/// data that is not base64 or a type haest cannot send is a line saying it was
/// omitted, and so is a part of any other kind. With no text at all, the
/// output is the `structuredContent` as JSON, else `(no content)`. Only
/// `isError: true` makes it an error output.
pub(crate) fn mcp_result_to_output(res: CallToolResult) -> ToolOutput {
    let mut media = Vec::new();
    let text = (res.content.iter().enumerate())
        .map(|(i, p)| part_text(p, i, &mut media))
        .collect::<Vec<_>>()
        .join("\n");
    let content = match res.structured_content {
        _ if !text.is_empty() => text,
        Some(structured) if !structured.is_null() => structured.to_string(),
        _ => "(no content)".to_string(),
    };
    ToolOutput {
        content,
        is_error: res.is_error == Some(true),
        media,
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::*;

    /// `res` as rmcp reads it off the wire.
    fn output(res: Value) -> ToolOutput {
        mcp_result_to_output(serde_json::from_value(res).unwrap())
    }

    #[test]
    fn joins_text_parts_flags_errors_and_an_image_with_no_data_is_a_line_not_media() {
        let out = output(json!({
            "content": [
                {"type": "text", "text": "a"},
                {"type": "image", "data": "", "mimeType": "image/png"},
            ],
            "isError": true,
        }));
        let parts: Vec<_> = out.content.split('\n').collect();
        assert_eq!(parts.len(), 2);
        assert_eq!(parts[0], "a");
        assert!(out.media.is_empty());
        assert!(out.is_error);
    }

    #[test]
    fn media_haest_cannot_send_is_a_line_not_media() {
        let out = output(json!({
            "content": [{"type": "image", "data": "AQID", "mimeType": "image/tiff"}],
        }));
        assert!(out.media.is_empty());
        assert_eq!(out.content.lines().count(), 1);
    }

    #[test]
    fn resources_show_their_text_and_links_are_one_line_each() {
        let out = output(json!({
            "content": [
                {"type": "resource", "resource": {"uri": "file:///a", "text": "body"}},
                {"type": "resource", "resource": {"uri": "file:///b", "blob": "AQID"}},
                {"type": "resource_link", "uri": "file:///c", "name": "c"},
            ],
        }));
        let lines: Vec<_> = out.content.lines().collect();
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[0], "body");
        assert!(lines[1].contains("file:///b"));
        assert!(out.media.is_empty());
    }

    #[test]
    fn no_text_falls_back_to_structured_content_then_to_a_placeholder() {
        let structured = output(json!({"structuredContent": {"n": 1}}));
        assert_eq!(
            serde_json::from_str::<Value>(&structured.content).unwrap(),
            json!({"n": 1})
        );
        let empty = output(json!({"content": []}));
        assert!(!empty.content.is_empty());
        assert!(!empty.is_error);
    }
}
