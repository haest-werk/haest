use super::*;
use crate::{deps::NoHooks, messages::ToolResultMessage};

fn png() -> MediaPart {
    media_from_bytes(&[1, 2, 3], "image/png", "shot.png").unwrap()
}

#[test]
fn kinds_and_names_map_to_what_providers_take() {
    assert_eq!(media_kind_of("image/png"), Some(MediaKind::Image));
    assert_eq!(media_kind_of("image/tiff"), None);
    assert_eq!(media_kind_of("application/pdf"), Some(MediaKind::File));
    assert_eq!(media_type_from_name("A.JPG"), Some("image/jpeg"));
    assert_eq!(media_type_from_name("notes.txt"), None);
}

#[test]
fn a_name_without_a_dot_has_no_extension() {
    assert_eq!(media_type_from_name("png"), None);
    assert_eq!(media_type_from_name("dir.d/pdf"), None);
    assert_eq!(media_type_from_name("dir/shot.PNG"), Some("image/png"));
}

#[test]
fn only_application_pdf_itself_is_a_file() {
    assert_eq!(media_kind_of("application/pdfx"), None);
    assert_eq!(media_kind_of("application/pdf-signed"), None);
}

#[test]
fn bytes_over_the_kind_limit_are_refused_with_a_reason() {
    let big = vec![0u8; 5 * 1024 * 1024 + 1];
    assert!(matches!(
        media_from_bytes(&big, "image/png", "big.png"),
        Err(MediaError::TooBig { size, limit, .. }) if size > limit
    ));
    assert!(matches!(
        media_from_bytes(b"x", "text/plain", "a.txt"),
        Err(MediaError::NotMedia { .. })
    ));
    assert_eq!(png().data, "AQID");
}

#[test]
fn a_text_only_model_gets_notes_in_place_of_media_and_the_history_is_untouched() {
    let history = vec![
        Message::User(UserMessage {
            content: "look".into(),
            media: vec![png()],
        }),
        Message::Tool(ToolResultMessage {
            tool_call_id: "c".into(),
            tool_name: "Read".into(),
            content: "".into(),
            is_error: false,
            media: vec![png()],
        }),
    ];
    let text_only: HashSet<_> = [InputModality::Text].into();
    let fitted = fit_for_model(&history, Some(&text_only), "m", &NoHooks);
    let Message::User(u) = &fitted[0] else {
        panic!()
    };
    assert!(u.media.is_empty() && u.content.starts_with("look\n\n[image"));
    let Message::Tool(t) = &fitted[1] else {
        panic!()
    };
    assert!(t.media.is_empty() && t.content.starts_with("[image"));
    assert_eq!(fit_media(&history, None, "m"), history);
}

#[test]
fn the_estimate_counts_media_as_tokens_not_base64() {
    let history = vec![Message::User(UserMessage {
        content: "x".into(),
        media: vec![png()],
    })];
    let (json, tokens) = messages_for_estimate(&history);
    assert_eq!(tokens, 1600);
    assert!(!json.contains("AQID"));
}
