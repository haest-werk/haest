//! What a loop event's payload carries of a message: never the media
//! bytes, thinking signatures or provider state. A path the OS allows but
//! JSON cannot hold goes out as text. That a mod file's handler receives
//! exactly each payload's JSON is haest-bridges' test.

use haest_core::{
    Agent, AgentDeps, AgentOptions, Message, SharedDeps, Tools, TurnCaps, Usage,
    messages::{AssistantPart, MediaKind, MediaPart, UserMessage},
    testing::{ScriptedProvider, text_answer},
    workspace::ShutWorkspace,
};
use serde_json::json;

use crate::{
    payload::{AgentView, ModelAfterPayload, ModelBeforePayload},
    testing::CWD,
};

/// A session file or working dir whose name is not UTF-8 (Latin-1 `é`)
/// would make every payload fail to encode, and every external hook with
/// it: the bytes JSON cannot hold go out as U+FFFD.
#[cfg(unix)]
#[test]
fn a_path_that_is_not_utf8_goes_out_with_its_bad_bytes_replaced() {
    use std::{ffi::OsStr, os::unix::ffi::OsStrExt, path::Path};

    use haest_core::ToolContext;

    use crate::payload::{SessionView, ToolCallContext};

    let odd = Path::new(OsStr::from_bytes(b"/home/r\xe9sum\xe9"));
    let session = SessionView::new("s-1").with_transcript_path(odd.join("s-1.jsonl"));
    assert_eq!(
        serde_json::to_value(session).unwrap(),
        json!({ "id": "s-1", "transcriptPath": "/home/r\u{FFFD}sum\u{FFFD}/s-1.jsonl" })
    );
    let ctx = ToolCallContext::new(&ToolContext::new(odd, ShutWorkspace));
    assert_eq!(
        serde_json::to_value(ctx).unwrap(),
        json!({ "cwd": "/home/r\u{FFFD}sum\u{FFFD}", "toolCallId": null })
    );
}

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
        json!({
            "modelSpec": "scripted:m", "effort": null, "historyLength": 0,
            "contextTokens": 0, "contextWindow": null, "lastMessage": null
        })
    );
}

/// A hook that compacts on size, not on a message count, reads how full the
/// context is: what the last answer's request held, out of the window.
#[test]
fn the_agent_view_says_how_full_the_context_is() {
    let mut answer = text_answer("done");
    answer.usage = Usage {
        input: 1_000,
        cache_read: 50_000,
        cache_write: 9_000,
        output: 400,
        ..Usage::default()
    };
    let history = vec![Message::user("go"), Message::Assistant(answer)];
    let p = ScriptedProvider::new([]);
    let shared = SharedDeps::new(p.resolver(), CWD, ShutWorkspace);
    let caps = TurnCaps {
        context_window: 200_000,
        ..TurnCaps::default()
    };
    let deps = AgentDeps::new(shared, Tools::new(), "").with_turn_caps(caps);
    let agent = Agent::new(deps, AgentOptions::new("scripted:m").with_history(history));

    let view = AgentView::new(&agent);
    assert_eq!(
        (view.context_tokens, view.context_window),
        (60_400, Some(200_000))
    );
    let v = serde_json::to_value(&view).unwrap();
    assert_eq!(
        (v["contextTokens"].clone(), v["contextWindow"].clone()),
        (json!(60_400), json!(200_000))
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
