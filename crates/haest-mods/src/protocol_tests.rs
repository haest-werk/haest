//! What a loop event's payload carries of a message: never the media
//! bytes, thinking signatures or provider state. That a mod file's handler
//! receives exactly each payload's JSON is haest-bridges' test.

use haest_core::{
    Agent, AgentDeps, AgentOptions, Message, SharedDeps, Tools,
    messages::{AssistantPart, MediaKind, MediaPart, UserMessage},
    testing::{ScriptedProvider, text_answer},
    workspace::ShutWorkspace,
};
use serde_json::json;

use crate::{
    payload::{ModelAfterPayload, ModelBeforePayload},
    testing::CWD,
};

fn agent_with(history: Vec<Message>) -> Agent {
    let p = ScriptedProvider::new([]);
    let shared = SharedDeps::new(p.resolver(), CWD, ShutWorkspace);
    let deps = AgentDeps::new(shared, Tools::new(), "");
    Agent::new(deps, AgentOptions::new("scripted:m").with_history(history))
}

#[test]
fn an_empty_history_hands_model_before_no_last_message() {
    let v = serde_json::to_value(ModelBeforePayload::new(&agent_with(Vec::new()))).unwrap();
    assert_eq!(
        v,
        json!({ "modelSpec": "scripted:m", "effort": null, "historyLength": 0, "lastMessage": null })
    );
}

#[test]
fn a_message_crosses_without_media_bytes_signatures_or_provider_state() {
    let photo = MediaPart {
        kind: MediaKind::Image,
        media_type: "image/png".into(),
        data: "QUJD".repeat(1_000),
        name: "photo.png".into(),
    };
    let asked = Message::User(UserMessage {
        content: "what is this?".into(),
        media: vec![photo],
    });
    let before = ModelBeforePayload::new(&agent_with(vec![asked]));
    let v = before.last_message.unwrap();
    assert_eq!(v["content"], "what is this?");
    assert_eq!(v["media"][0]["name"], "photo.png");
    assert!(v["media"][0].get("data").is_none());

    let mut answer = text_answer("a cat");
    answer.content.insert(
        0,
        AssistantPart::Thinking {
            thinking: "looks furry".into(),
            signature: Some("sig".repeat(100)),
            redacted: Some("opaque".into()),
        },
    );
    answer.provider_data = Some(json!({"reasoning": "opaque"}));
    let v = ModelAfterPayload::new(&agent_with(Vec::new()), &answer).answer;
    assert_eq!(v["content"][0]["thinking"], "looks furry");
    assert_eq!(v["content"][1]["text"], "a cat");
    for gone in [
        &v["content"][0]["signature"],
        &v["content"][0]["redacted"],
        &v["providerData"],
    ] {
        assert!(gone.is_null(), "{v}");
    }
}
