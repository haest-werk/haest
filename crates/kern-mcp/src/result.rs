//! An MCP `tools/call` result as a kern [`ToolOutput`]. Works on the JSON shape,
//! not the SDK's types, so a part the SDK would refuse (an image with no data, a
//! type it does not know) still becomes a line the model can read.

use base64::{
    Engine as _,
    alphabet::STANDARD,
    engine::{DecodePaddingMode, GeneralPurpose, GeneralPurposeConfig},
};
use kern_core::{
    media::media_from_bytes,
    messages::{MediaKind, MediaPart},
    tool::ToolOutput,
};
use serde_json::Value;

fn field<'a>(p: &'a Value, key: &str) -> Option<&'a str> {
    p.get(key).and_then(Value::as_str)
}

fn part_type(p: &Value) -> &str {
    field(p, "type").unwrap_or("unknown")
}

fn kind_name(kind: MediaKind) -> &'static str {
    match kind {
        MediaKind::Image => "image",
        MediaKind::File => "file",
        MediaKind::Audio => "audio",
        MediaKind::Video => "video",
    }
}

/// Base64 the way a lenient decoder reads it: padding optional, whitespace ignored.
fn decode_base64(data: &str) -> Option<Vec<u8>> {
    let engine = GeneralPurpose::new(
        &STANDARD,
        GeneralPurposeConfig::new().with_decode_padding_mode(DecodePaddingMode::Indifferent),
    );
    let compact: String = data.chars().filter(|c| !c.is_ascii_whitespace()).collect();
    engine.decode(compact).ok()
}

/// An MCP image/audio part becomes media (collected into `media`); the model reads one line in its place.
fn mcp_media(p: &Value, index: usize, media: &mut Vec<MediaPart>) -> String {
    let kind = part_type(p);
    let (Some(data), Some(mime)) = (field(p, "data"), field(p, "mimeType")) else {
        return format!("[{kind} omitted: no data]");
    };
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
            let line = format!("[{} {} attached]", kind_name(part.kind), part.name);
            media.push(part);
            line
        }
        Err(reason) => format!("[{reason}]"),
    }
}

fn part_text(p: &Value, index: usize, media: &mut Vec<MediaPart>) -> String {
    match part_type(p) {
        "text" => field(p, "text").unwrap_or("").to_string(),
        "resource" => {
            let resource = p.get("resource");
            match resource.and_then(|r| field(r, "text")) {
                Some(text) => text.to_string(),
                None => {
                    let uri = resource.and_then(|r| field(r, "uri")).unwrap_or("");
                    format!("[resource {uri}]")
                }
            }
        }
        "image" | "audio" => mcp_media(p, index, media),
        other => match field(p, "mimeType").filter(|m| !m.is_empty()) {
            Some(mime) => format!("[{other} {mime} omitted]"),
            None => format!("[{other} omitted]"),
        },
    }
}

/// JavaScript truthiness, for "is there structured content worth showing".
fn truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|f| f != 0.0 && !f.is_nan()),
        Value::String(s) => !s.is_empty(),
        Value::Array(_) | Value::Object(_) => true,
    }
}

/// Text parts joined by newlines; image/audio parts as media plus a line each;
/// no text at all falls back to the structured content, then to `(no content)`.
/// Only `isError: true` is an error.
pub fn mcp_result_to_output(res: &Value) -> ToolOutput {
    let parts = res
        .get("content")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default();
    let mut media = Vec::new();
    let text = parts
        .iter()
        .enumerate()
        .map(|(i, p)| part_text(p, i, &mut media))
        .collect::<Vec<_>>()
        .join("\n");
    let content = if !text.is_empty() {
        text
    } else {
        match res.get("structuredContent").filter(|v| truthy(v)) {
            Some(structured) => structured.to_string(),
            None => "(no content)".to_string(),
        }
    };
    ToolOutput {
        content,
        is_error: res.get("isError") == Some(&Value::Bool(true)),
        media,
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn joins_text_parts_flags_errors_and_an_image_with_no_data_is_a_line_not_media() {
        let out = mcp_result_to_output(&json!({
            "content": [
                {"type": "text", "text": "a"},
                {"type": "image", "mimeType": "image/png"},
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
    fn an_mcp_tool_returning_an_image_hands_the_model_the_image_as_media() {
        let out = mcp_result_to_output(&json!({
            "content": [
                {"type": "text", "text": "screenshot taken"},
                {"type": "image", "data": "AQID", "mimeType": "image/png"},
            ],
        }));
        assert_eq!(
            out.media,
            [MediaPart {
                kind: MediaKind::Image,
                media_type: "image/png".into(),
                data: "AQID".into(),
                name: "image-2.png".into(),
            }]
        );
        assert!(!out.is_error);
    }

    #[test]
    fn media_kern_cannot_send_is_a_line_not_media() {
        let out = mcp_result_to_output(&json!({
            "content": [{"type": "image", "data": "AQID", "mimeType": "image/tiff"}],
        }));
        assert!(out.media.is_empty());
        assert_eq!(out.content.lines().count(), 1);
    }

    #[test]
    fn resources_show_their_text_and_unknown_parts_are_one_line_each() {
        let out = mcp_result_to_output(&json!({
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
        let structured = mcp_result_to_output(&json!({"structuredContent": {"n": 1}}));
        assert_eq!(
            serde_json::from_str::<Value>(&structured.content).unwrap(),
            json!({"n": 1})
        );
        let empty = mcp_result_to_output(&json!({"content": []}));
        assert!(!empty.content.is_empty());
        assert!(!empty.is_error);
    }
}
