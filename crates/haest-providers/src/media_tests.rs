//! Media in all three wire formats.

use haest_core::{
    cache::CacheTtl,
    messages::{
        AssistantMessage, AssistantPart, MediaKind, MediaPart, Message, StopReason, ToolCall,
        ToolResultMessage, UserMessage,
    },
};
use serde_json::{Value, json};

/// The default cache TTL: media is converted the same under either.
const FIVE: CacheTtl = CacheTtl::FiveMinutes;

use crate::{
    anthropic::to_anthropic_messages,
    compat::{CompatFlavor, to_chat_messages},
    responses::convert::to_responses_input,
};

fn part(kind: MediaKind, media_type: &str, data: &str, name: &str) -> MediaPart {
    MediaPart {
        kind,
        media_type: media_type.into(),
        data: data.into(),
        name: name.into(),
    }
}

fn img() -> MediaPart {
    part(MediaKind::Image, "image/png", "AAAA", "a.png")
}

fn pdf() -> MediaPart {
    part(MediaKind::File, "application/pdf", "BBBB", "b.pdf")
}

fn user(text: &str, media: Vec<MediaPart>) -> Message {
    Message::User(UserMessage {
        content: text.into(),
        media,
    })
}

fn tool(id: &str, content: &str, media: Vec<MediaPart>) -> Message {
    Message::Tool(ToolResultMessage {
        tool_call_id: id.into(),
        tool_name: "Read".into(),
        content: content.into(),
        is_error: false,
        media,
    })
}

/// An assistant that called two tools at once, both of which returned media.
fn parallel_tools() -> Vec<Message> {
    let mut a = AssistantMessage::start("p", "m");
    let call = |id: &str| {
        AssistantPart::ToolCall(ToolCall {
            id: id.into(),
            name: "Read".into(),
            input: Default::default(),
        })
    };
    a.content = vec![call("t1"), call("t2")];
    a.stop_reason = StopReason::ToolUse;
    vec![
        Message::user("compare them"),
        Message::Assistant(a),
        tool("t1", "one", vec![img()]),
        tool("t2", "two", vec![pdf()]),
    ]
}

fn types(v: &Value) -> Vec<String> {
    v.as_array()
        .unwrap()
        .iter()
        .map(|b| b["type"].as_str().unwrap().to_string())
        .collect()
}

// -- Anthropic

#[test]
fn anthropic_a_user_turn_carries_text_then_an_image_block_then_a_document_block() {
    let msgs = to_anthropic_messages(
        &[user("look", vec![img(), pdf()])],
        "anthropic",
        FIVE,
        false,
    );
    let blocks = &msgs[0]["content"];
    assert_eq!(types(blocks), ["text", "image", "document"]);
    let data: Vec<_> = blocks
        .as_array()
        .unwrap()
        .iter()
        .map(|b| b.pointer("/source/data").cloned())
        .collect();
    assert_eq!(data, [None, Some(json!("AAAA")), Some(json!("BBBB"))]);
}

#[test]
fn anthropic_tool_media_rides_inside_the_tool_result() {
    let out = to_anthropic_messages(&parallel_tools(), "anthropic", FIVE, false);
    let results = &out[2]["content"];
    assert_eq!(types(results), ["tool_result", "tool_result"]);
    assert_eq!(types(&results[0]["content"]), ["text", "image"]);
    assert_eq!(types(&results[1]["content"]), ["text", "document"]);
}

#[test]
fn anthropic_gets_no_audio_it_becomes_a_text_block() {
    let audio = part(MediaKind::Audio, "audio/wav", "CC", "x.wav");
    let msgs = to_anthropic_messages(&[user("hear", vec![audio])], "anthropic", FIVE, false);
    assert_eq!(types(&msgs[0]["content"]), ["text", "text"]);
}

// -- Chat Completions (OpenRouter)

fn chat(messages: &[Message]) -> Vec<Value> {
    to_chat_messages(
        "sys",
        messages,
        CompatFlavor::OpenRouter,
        "z-ai/glm-5.3-flash",
        FIVE,
        "openrouter",
    )
}

#[test]
fn chat_a_user_turn_with_no_media_stays_a_plain_string_on_the_wire() {
    assert_eq!(
        chat(&[Message::user("hi")])[1],
        json!({"role": "user", "content": "hi"})
    );
}

#[test]
fn chat_each_media_kind_maps_to_its_openrouter_part_type() {
    let audio = part(MediaKind::Audio, "audio/mpeg", "CC", "x.mp3");
    let video = part(MediaKind::Video, "video/mp4", "DD", "x.mp4");
    let out = chat(&[user("all", vec![img(), pdf(), audio, video])]);
    let parts = &out[1]["content"];
    assert_eq!(
        types(parts),
        ["text", "image_url", "file", "input_audio", "video_url"]
    );
    assert_eq!(
        parts[1]["image_url"],
        json!({"url": "data:image/png;base64,AAAA"})
    );
    assert_eq!(
        parts[3]["input_audio"],
        json!({"data": "CC", "format": "mp3"})
    );
}

#[test]
fn chat_tool_media_lands_in_one_user_message_after_the_whole_run_of_tool_results() {
    let out = chat(&parallel_tools());
    let roles: Vec<_> = out.iter().map(|m| m["role"].as_str().unwrap()).collect();
    assert_eq!(
        roles,
        ["system", "user", "assistant", "tool", "tool", "user"]
    );
    assert_eq!(types(&out[5]["content"]), ["text", "image_url", "file"]);
}

// -- OpenAI Responses

#[test]
fn responses_images_and_pdfs_become_input_image_and_input_file() {
    let items = to_responses_input(&[user("look", vec![img(), pdf()])], "openai");
    assert_eq!(
        types(&items[0]["content"]),
        ["input_text", "input_image", "input_file"]
    );
}

#[test]
fn responses_tool_media_lands_in_one_user_item_after_the_function_call_outputs() {
    let out = to_responses_input(&parallel_tools(), "openai");
    let kinds: Vec<_> = out
        .iter()
        .map(|i| {
            i.get("type")
                .or_else(|| i.get("role"))
                .and_then(Value::as_str)
                .unwrap()
        })
        .collect();
    assert_eq!(
        kinds,
        [
            "user",
            "function_call",
            "function_call",
            "function_call_output",
            "function_call_output",
            "user"
        ]
    );
}
