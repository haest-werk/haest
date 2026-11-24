//! The wire's bytes, pinned. Every literal here is what haest-wire wrote
//! before its Rust API was reworked (the first 0.3.0 cut): other programs
//! speak this format, so a change to the types must leave each line as it
//! is, and each line must read back as the value that wrote it.

use haest_wire::{
    Access, Answer, Answerer, Control, Envelope, Framer, Inbound, Level, Origin, Outbound, Path,
    Request, Snapshot, State, UnknownWord, decode,
};
use serde::{Serialize, de::DeserializeOwned};
use serde_json::json;

/// `value` writes `line`, and `line` reads back as `value`.
fn pinned<T>(value: &T, line: &str)
where
    T: Serialize + DeserializeOwned + PartialEq + std::fmt::Debug,
{
    assert_eq!(serde_json::to_string(value).unwrap(), line);
    assert_eq!(&serde_json::from_str::<T>(line).unwrap(), value, "{line}");
}

#[test]
fn every_request_is_the_line_it_was() {
    let requests = [
        (
            Request::Control(Control::Interrupt),
            r#"{"v":1,"id":"r1","type":"control","verb":"interrupt"}"#,
        ),
        (
            Request::Control(Control::SetModel {
                model: "a:b".into(),
            }),
            r#"{"v":1,"id":"r1","type":"control","verb":"set_model","model":"a:b"}"#,
        ),
        (
            Request::Control(Control::SetEffort {
                effort: Some("high".into()),
            }),
            r#"{"v":1,"id":"r1","type":"control","verb":"set_effort","effort":"high"}"#,
        ),
        (
            Request::Control(Control::SetEffort { effort: None }),
            r#"{"v":1,"id":"r1","type":"control","verb":"set_effort"}"#,
        ),
        (
            Request::Control(Control::Clear),
            r#"{"v":1,"id":"r1","type":"control","verb":"clear"}"#,
        ),
        (
            Request::Control(Control::Close),
            r#"{"v":1,"id":"r1","type":"control","verb":"close"}"#,
        ),
        (
            Request::Turn { text: "hi".into() },
            r#"{"v":1,"id":"r1","type":"turn","text":"hi"}"#,
        ),
        (
            Request::ApprovalAnswer {
                ask: 3,
                answer: Answer::Always,
            },
            r#"{"v":1,"id":"r1","type":"approval_answer","ask":3,"answer":"always"}"#,
        ),
        (Request::Snapshot, r#"{"v":1,"id":"r1","type":"snapshot"}"#),
        (Request::History, r#"{"v":1,"id":"r1","type":"history"}"#),
        (
            Request::Events {
                after: 5,
                follow: true,
            },
            r#"{"v":1,"id":"r1","type":"events","after":5,"follow":true}"#,
        ),
    ];
    for (request, line) in requests {
        let inbound = Inbound::new("r1", request);
        pinned(&inbound, line);
        assert_eq!(decode(line).unwrap(), inbound, "{line}");
    }
}

#[test]
fn every_outbound_frame_is_the_line_it_was() {
    let frames = [
        Outbound::Notice {
            level: Level::Info,
            text: "n".into(),
        },
        Outbound::Notice {
            level: Level::Warning,
            text: "w".into(),
        },
        Outbound::Notice {
            level: Level::Error,
            text: "e".into(),
        },
        Outbound::Ask {
            text: "a".into(),
            approval: None,
        },
        Outbound::Status(Snapshot {
            state: State::Running,
            session: "x".into(),
            model_spec: "m".into(),
            effort: Some("high".into()),
            usd: Some(0.25),
            queued: 1,
            messages: 2,
            last_seq: 3,
        }),
        Outbound::Status(Snapshot::default()),
        Outbound::ApprovalAsk {
            ask: 3,
            tool_name: "Bash".into(),
            tool_call_id: None,
            description: "d".into(),
            preview: "p".into(),
            key: Some("k".into()),
        },
        Outbound::ApprovalWithdrawn { ask: 3 },
        Outbound::SubagentStarted {
            name: "reader".into(),
            parent: "main".into(),
            description: "d".into(),
        },
        Outbound::SubagentStopped { ok: true },
        Outbound::Hello {
            session: "x".into(),
            model_spec: "m".into(),
            pid: 7,
            access: Access::Command,
            token: Some("t".into()),
        },
        Outbound::Hello {
            session: "x".into(),
            model_spec: "m".into(),
            pid: 7,
            access: Access::Watch,
            token: None,
        },
        Outbound::Gap {
            after: 4,
            missed: Some(2),
        },
        Outbound::done("r1", Some(json!({"n": 1}))),
        Outbound::refused("r2", "no"),
    ];
    let lines = [
        r#"{"v":1,"sid":"s1","seq":1,"ts":1000,"path":["main"],"type":"notice","level":"info","text":"n"}"#,
        r#"{"v":1,"sid":"s1","seq":2,"ts":1000,"path":["main"],"type":"notice","level":"warning","text":"w"}"#,
        r#"{"v":1,"sid":"s1","seq":3,"ts":1000,"path":["main"],"type":"notice","level":"error","text":"e"}"#,
        r#"{"v":1,"sid":"s1","seq":4,"ts":1000,"path":["main"],"type":"ask","text":"a"}"#,
        r#"{"v":1,"sid":"s1","seq":5,"ts":1000,"path":["main"],"type":"status","state":"running","session":"x","model":"m","effort":"high","usd":0.25,"queued":1,"messages":2,"lastSeq":3}"#,
        r#"{"v":1,"sid":"s1","seq":6,"ts":1000,"path":["main"],"type":"status","state":"idle","session":"","model":"","queued":0,"messages":0,"lastSeq":0}"#,
        r#"{"v":1,"sid":"s1","seq":7,"ts":1000,"path":["main"],"type":"approval_ask","ask":3,"toolName":"Bash","description":"d","preview":"p","key":"k"}"#,
        r#"{"v":1,"sid":"s1","seq":8,"ts":1000,"path":["main"],"type":"approval_withdrawn","ask":3}"#,
        r#"{"v":1,"sid":"s1","seq":9,"ts":1000,"path":["main"],"type":"subagent_started","name":"reader","parent":"main","description":"d"}"#,
        r#"{"v":1,"sid":"s1","seq":10,"ts":1000,"path":["main"],"type":"subagent_stopped","ok":true}"#,
        r#"{"v":1,"sid":"s1","seq":11,"ts":1000,"path":["main"],"type":"hello","session":"x","model":"m","pid":7,"access":"command","token":"t"}"#,
        r#"{"v":1,"sid":"s1","seq":12,"ts":1000,"path":["main"],"type":"hello","session":"x","model":"m","pid":7,"access":"watch"}"#,
        r#"{"v":1,"sid":"s1","seq":13,"ts":1000,"path":["main"],"type":"gap","after":4,"missed":2}"#,
        r#"{"v":1,"sid":"s1","seq":14,"ts":1000,"path":["main"],"type":"reply","id":"r1","ok":true,"result":{"n":1}}"#,
        r#"{"v":1,"sid":"s1","seq":15,"ts":1000,"path":["main"],"type":"reply","id":"r2","ok":false,"error":"no"}"#,
    ];
    let mut framer = Framer::new("s1").with_clock(|| 1000);
    for (frame, line) in frames.into_iter().zip(lines) {
        pinned(&framer.stamp(Path::main(), frame), line);
    }
    let sub = framer.stamp(
        Path::subagent("a1"),
        Outbound::SubagentStopped { ok: false },
    );
    pinned(
        &sub,
        r#"{"v":1,"sid":"s1","seq":16,"ts":1000,"path":["main","a1"],"type":"subagent_stopped","ok":false}"#,
    );
    let gap = Outbound::Gap {
        after: 9,
        missed: None,
    };
    let conn = Envelope {
        ts: 5,
        ..Envelope::connection("s1", 9, gap)
    };
    pinned(
        &conn,
        r#"{"v":1,"sid":"s1","seq":9,"ts":5,"path":["main"],"type":"gap","after":9}"#,
    );
}

/// What the approval asks gained once a conversation at a terminal also
/// puts them to its peers: the call an ask holds up, the terminal question
/// that is its other side, and who answered. Each is a field or a frame of
/// its own, so a reader of the lines above reads these as before.
#[test]
fn the_approval_frames_added_since_are_the_lines_they_are() {
    let frames = [
        Outbound::ApprovalAsk {
            ask: 3,
            tool_name: "Bash".into(),
            tool_call_id: Some("call-1".into()),
            description: "d".into(),
            preview: "p".into(),
            key: None,
        },
        Outbound::Ask {
            text: "a".into(),
            approval: Some(3),
        },
        Outbound::ApprovalAnswered {
            ask: 3,
            answer: Answer::Deny,
            by: Answerer::Terminal,
        },
        Outbound::ApprovalAnswered {
            ask: 4,
            answer: Answer::Always,
            by: Answerer::Peer,
        },
    ];
    let lines = [
        r#"{"v":1,"sid":"s1","seq":1,"ts":1000,"path":["main"],"type":"approval_ask","ask":3,"toolName":"Bash","toolCallId":"call-1","description":"d","preview":"p"}"#,
        r#"{"v":1,"sid":"s1","seq":2,"ts":1000,"path":["main"],"type":"ask","text":"a","approval":3}"#,
        r#"{"v":1,"sid":"s1","seq":3,"ts":1000,"path":["main"],"type":"approval_answered","ask":3,"answer":"deny","by":"terminal"}"#,
        r#"{"v":1,"sid":"s1","seq":4,"ts":1000,"path":["main"],"type":"approval_answered","ask":4,"answer":"always","by":"peer"}"#,
    ];
    let mut framer = Framer::new("s1").with_clock(|| 1000);
    for (frame, line) in frames.into_iter().zip(lines) {
        pinned(&framer.stamp(Path::main(), frame), line);
    }
}

/// Each word is the JSON string it was, and its text both ways.
fn word<T>(value: T, word: &str)
where
    T: Serialize + DeserializeOwned + PartialEq + std::fmt::Debug + std::fmt::Display,
    T: std::str::FromStr<Err = UnknownWord>,
{
    pinned(&value, &format!("\"{word}\""));
    assert_eq!(value.to_string(), word);
    assert_eq!(
        format!("{value:>9}|"),
        format!("{word:>9}|"),
        "a width pads"
    );
    assert_eq!(word.parse::<T>(), Ok(value), "{word}");
}

#[test]
fn every_word_is_the_word_it_was() {
    word(Origin::Human, "human");
    word(Origin::Agent, "agent");
    word(Answer::Allow, "allow");
    word(Answer::Always, "always");
    word(Answer::Deny, "deny");
    word(State::Idle, "idle");
    word(State::Running, "running");
    word(Access::Watch, "watch");
    word(Access::Command, "command");
    word(Level::Info, "info");
    word(Level::Warning, "warning");
    word(Level::Error, "error");
    word(Answerer::Terminal, "terminal");
    word(Answerer::Peer, "peer");
}

/// A word a type does not know is refused the same way as text and as JSON.
#[test]
fn an_unknown_word_names_the_words_there_are() {
    let said = r#"unknown answer "maybe" (allow, always, deny)"#;
    assert_eq!("maybe".parse::<Answer>().unwrap_err().to_string(), said);
    let json = serde_json::from_str::<Answer>(r#""maybe""#).unwrap_err();
    assert!(json.to_string().starts_with(said), "{json}");
    let line = r#"{"v":1,"id":"x","type":"approval_answer","ask":1,"answer":"maybe"}"#;
    let shape = decode(line).unwrap_err();
    assert!(
        std::error::Error::source(&shape).is_some_and(|cause| cause.to_string().starts_with(said)),
        "{shape:?}"
    );
}
