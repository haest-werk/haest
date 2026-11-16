//! A log that ends mid-tool (the process was killed): every replay answers
//! the calls left without a result, so a resumed conversation is one a
//! provider accepts. Ported from the TS haest's session-store tests.

use super::*;
use crate::messages::{AssistantMessage, AssistantPart, StopReason, ToolCall, ToolResultMessage};

fn calls(ids: &[&str]) -> Message {
    let mut m = AssistantMessage::start("fake", "m");
    m.content = ids
        .iter()
        .map(|id| {
            AssistantPart::ToolCall(ToolCall {
                id: (*id).into(),
                name: "Bash".into(),
                input: Default::default(),
            })
        })
        .collect();
    m.stop_reason = StopReason::ToolUse;
    Message::Assistant(m)
}

fn result(id: &str) -> Message {
    Message::Tool(ToolResultMessage {
        tool_call_id: id.into(),
        tool_name: "Bash".into(),
        content: "ok".into(),
        is_error: false,
        media: Vec::new(),
    })
}

/// Each message as a short tag: u:<text>, a:<call ids>, t:<id>[!] (! = error).
fn shape(messages: &[Message]) -> Vec<String> {
    messages
        .iter()
        .map(|m| match m {
            Message::User(u) => format!("u:{}", u.content),
            Message::Tool(t) => {
                format!("t:{}{}", t.tool_call_id, if t.is_error { "!" } else { "" })
            }
            Message::Assistant(a) => {
                let ids: Vec<&str> = a.tool_calls().map(|c| c.id.as_str()).collect();
                format!("a:{}", ids.join(","))
            }
        })
        .collect()
}

fn appended(s: &JsonlSessionFile, messages: impl IntoIterator<Item = Message>) {
    for message in messages {
        s.append(&SessionRecord::Message { t: 0, message });
    }
}

#[tokio::test]
async fn every_unanswered_call_is_answered_as_an_interrupted_error_after_its_answered_siblings() {
    let fs = Arc::new(ModeFs::default());
    let s = store(&fs);
    s.create("c1", "/x", 0).await.unwrap();
    appended(&s, [Message::user("go"), calls(&["A", "B"]), result("A")]);
    s.flush().await;
    let before = fs.read_to_string(s.path()).await.unwrap();
    let back = s.load().await.unwrap();
    assert_eq!(shape(&back.messages), ["u:go", "a:A,B", "t:A", "t:B!"]);
    let Message::Tool(answer) = &back.messages[3] else {
        panic!("a tool result")
    };
    assert_eq!(answer.tool_name, "Bash");
    assert_eq!(
        fs.read_to_string(s.path()).await.unwrap(),
        before,
        "the file is untouched"
    );

    // The resumed turn's messages follow in the file; the answer still lands before them.
    appended(&s, [Message::user("again")]);
    let back = s.load().await.unwrap();
    assert_eq!(
        shape(&back.messages),
        ["u:go", "a:A,B", "t:A", "t:B!", "u:again"]
    );
}

#[tokio::test]
async fn a_later_compaction_keeps_the_tail_the_resumed_conversation_kept() {
    let fs = Arc::new(ModeFs::default());
    let s = store(&fs);
    s.create("c1", "/x", 0).await.unwrap();
    let next = [Message::user("next"), Message::user("more")];
    appended(
        &s,
        [Message::user("go"), calls(&["A"])].into_iter().chain(next),
    );
    // In memory the resumed history was [go, a:A, t:A!, next, more]; the cut kept its last 4.
    s.append(&SessionRecord::Compaction {
        t: 0,
        summary: "SUM".into(),
        kept: 4,
    });
    let back = s.load().await.unwrap();
    assert_eq!(
        shape(&back.messages[1..]),
        ["a:A", "t:A!", "u:next", "u:more"]
    );
}
