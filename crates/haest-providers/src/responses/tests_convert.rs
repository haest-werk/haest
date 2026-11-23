use haest_core::{
    messages::{AssistantMessage, AssistantPart, Message, StopReason, ToolCall, ToolResultMessage},
    provider::{Effort, ToolSpec},
};
use serde_json::{Value, json};

use super::{
    convert::{reasoning_params, to_responses_input},
    *,
};
use crate::testing::request;

fn assistant(provider: &str) -> AssistantMessage {
    let mut m = AssistantMessage::start(provider, "gpt-5.5");
    m.content = vec![
        AssistantPart::Thinking {
            thinking: "plan".into(),
            signature: Some("ENC1".into()),
            redacted: None,
        },
        AssistantPart::Text { text: "ok".into() },
        AssistantPart::ToolCall(ToolCall {
            id: "call_1".into(),
            name: "Read".into(),
            input: json!({"file_path": "a"}).as_object().unwrap().clone(),
        }),
    ];
    m.stop_reason = StopReason::ToolUse;
    m
}

fn tool(id: &str, name: &str, content: &str, is_error: bool) -> Message {
    Message::Tool(ToolResultMessage {
        tool_call_id: id.into(),
        tool_name: name.into(),
        content: content.into(),
        is_error,
        media: Vec::new(),
    })
}

fn history(provider: &str) -> Vec<Message> {
    vec![
        Message::user("go"),
        Message::Assistant(assistant(provider)),
        tool("call_1", "Read", "data", false),
        tool("call_2", "Bash", "", true),
    ]
}

#[test]
fn carries_function_tools_and_reasoning_effort_together_stateless_system_in_instructions() {
    let mut req = request("gpt-5.5");
    req.effort = Some(Effort::Low);
    req.max_tokens = Some(999);
    req.tools = vec![ToolSpec {
        name: "Read".into(),
        description: "read a file".into(),
        parameters: json!({"type": "object", "properties": {}}),
    }];
    let body = build_responses_body(&ResponsesOptions::new("openai", "k"), &req);
    assert_eq!(
        body["tools"],
        json!([{
            "type": "function", "name": "Read", "description": "read a file",
            "parameters": {"type": "object", "properties": {}}, "strict": false,
        }])
    );
    assert_eq!(
        body["reasoning"],
        json!({"effort": "low", "summary": "auto"})
    );
    assert_eq!(body["instructions"], json!("sys"));
    assert_eq!(body["store"], json!(false));
    assert_eq!(body["include"], json!(["reasoning.encrypted_content"]));
    assert_eq!(body["stream"], json!(true));
    assert_eq!(body["max_output_tokens"], json!(999));
    assert_eq!(body["model"], json!("gpt-5.5"));
}

#[test]
fn no_tools_and_no_effort_send_neither_key() {
    let body = build_responses_body(&ResponsesOptions::new("openai", "k"), &request("gpt-5.5"));
    assert!(body.get("tools").is_none());
    assert!(body.get("reasoning").is_none());
    assert!(body.get("max_output_tokens").is_none());
}

#[test]
fn our_effort_scale_clamps_onto_the_values_the_api_accepts() {
    let effort_of = |e| reasoning_params(Some(e)).map(|r| r["effort"].clone());
    assert_eq!(effort_of(Effort::Off), Some(json!("none")));
    assert_eq!(effort_of(Effort::Medium), Some(json!("medium")));
    assert_eq!(effort_of(Effort::High), Some(json!("high")));
    assert_eq!(effort_of(Effort::Xhigh), Some(json!("xhigh")));
    assert_eq!(effort_of(Effort::Max), Some(json!("xhigh")));
    assert_eq!(reasoning_params(None), None);
}

#[test]
fn user_text_assistant_text_tool_calls_and_tool_results_map_to_input_items_in_order() {
    assert_eq!(
        Value::Array(to_responses_input(&history("openai"), "openai")),
        json!([
            {"role": "user", "content": "go"},
            {"type": "reasoning", "summary": [{"type": "summary_text", "text": "plan"}], "encrypted_content": "ENC1"},
            {"role": "assistant", "content": "ok"},
            {"type": "function_call", "call_id": "call_1", "name": "Read", "arguments": "{\"file_path\":\"a\"}"},
            {"type": "function_call_output", "call_id": "call_1", "output": "data"},
            {"type": "function_call_output", "call_id": "call_2", "output": "(empty)"},
        ])
    );
}

#[test]
fn reasoning_replays_only_to_the_provider_that_produced_it_and_only_with_encrypted_content() {
    let foreign = to_responses_input(&history("openrouter"), "openai");
    assert!(!foreign.iter().any(|i| i["type"] == json!("reasoning")));
    let mut bare = assistant("openai");
    bare.content = vec![AssistantPart::Thinking {
        thinking: "no enc".into(),
        signature: None,
        redacted: None,
    }];
    assert!(to_responses_input(&[Message::Assistant(bare)], "openai").is_empty());
}

#[test]
fn an_empty_reasoning_summary_replays_as_an_empty_summary_list() {
    let mut msg = assistant("openai");
    msg.content = vec![AssistantPart::Thinking {
        thinking: String::new(),
        signature: Some("ENC2".into()),
        redacted: None,
    }];
    assert_eq!(
        Value::Array(to_responses_input(&[Message::Assistant(msg)], "openai")),
        json!([{"type": "reasoning", "summary": [], "encrypted_content": "ENC2"}])
    );
}
