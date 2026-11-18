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

/// `message`'s record, as the file holds it.
fn line(message: Message) -> String {
    serde_json::to_string(&SessionRecord::Message { t: 0, message }).unwrap()
}

/// What a disk that filled up mid-record kept of an answer's record.
const TORN_ANSWER: &str = r#"{"type":"message","t":0,"message":{"role":"assis"#;

/// A full disk cut an answer's record short, and its tool's result landed
/// once there was room again: no answer in the file has its call, and every
/// provider refuses a result with no call. It is dropped, and counted with
/// the torn line.
#[test]
fn a_result_whose_call_never_landed_is_dropped_and_counted() {
    let text = [
        line(Message::user("go")),
        line(calls(&["A"])),
        line(result("A")),
        TORN_ANSWER.into(),
        line(result("B")),
        line(Message::user("go on")),
    ]
    .join("\n");
    let back = replay(&text);
    assert_eq!(shape(&back.messages), ["u:go", "a:A", "t:A", "u:go on"]);
    assert_eq!(back.corrupt_lines, 2, "the torn answer, and its result");
}

/// The answer lost was the turn's first: its result follows the human's
/// message, and goes the same way, counted once also where a later
/// compaction summarised it.
#[test]
fn a_result_that_follows_no_answer_is_dropped_and_counted_once() {
    let head = [
        line(Message::user("go")),
        TORN_ANSWER.into(),
        line(result("B")),
        line(calls(&["C"])),
        line(result("C")),
    ]
    .join("\n");
    let next = line(Message::user("next"));
    let back = replay(&format!("{head}\n{next}"));
    assert_eq!(shape(&back.messages), ["u:go", "a:C", "t:C", "u:next"]);
    assert_eq!(back.corrupt_lines, 2, "the torn answer, and its result");

    // The compaction kept the last answer and its result.
    let compaction = SessionRecord::Compaction {
        t: 0,
        summary: "SUM".into(),
        kept: 2,
    };
    let compaction = serde_json::to_string(&compaction).unwrap();
    let back = replay(&format!("{head}\n{compaction}\n{next}"));
    assert_eq!(shape(&back.messages[1..]), ["a:C", "t:C", "u:next"]);
    assert_eq!(back.corrupt_lines, 2, "the torn answer, and its result");
}

#[test]
fn calls_and_their_results_replay_untouched() {
    let history = [
        Message::user("go"),
        calls(&["A", "B"]),
        result("A"),
        result("B"),
        Message::user("next"),
        calls(&["C"]),
        result("C"),
    ];
    let back = replay(&history.clone().map(line).join("\n"));
    assert_eq!(back.messages, history);
    assert_eq!(back.corrupt_lines, 0);
}

#[tokio::test]
async fn every_unanswered_call_is_answered_as_an_interrupted_error_after_its_answered_siblings() {
    let fs = Arc::new(ModeFs::default());
    let s = store(&fs);
    s.create("c1", std::path::Path::new("/x"), 0).await.unwrap();
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
    s.create("c1", std::path::Path::new("/x"), 0).await.unwrap();
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

/// The disk filled up as the answer's record was written, and the tool
/// then freed room (`rm -rf target/`), so its result landed whole: the
/// resumed history holds neither, and both are counted.
#[tokio::test]
async fn a_result_written_after_its_answer_failed_to_land_is_dropped() {
    let fs = Arc::new(ModeFs::default());
    let s = store(&fs);
    s.create("c1", std::path::Path::new("/x"), 0).await.unwrap();
    appended(&s, [Message::user("go")]);
    s.flush().await;
    *fs.short_append.lock().unwrap() = Some(20);
    appended(&s, [calls(&["A"]), result("A"), Message::user("go on")]);
    s.flush().await;

    let back = store(&fs).load().await.unwrap();
    assert_eq!(shape(&back.messages), ["u:go", "u:go on"]);
    assert_eq!(
        back.corrupt_lines, 2,
        "the answer cut short, and its result"
    );
}
