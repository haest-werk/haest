use haest::{AssistantMessage, ToolOutput, ToolResultMessage};
use serde_json::json;

use super::*;

fn call(name: &str, input: Value) -> ToolCall {
    ToolCall {
        id: "c".into(),
        name: name.into(),
        input: input.as_object().cloned().unwrap(),
    }
}

/// `Read src/a.rs`, done in 3 ms; failed or not.
pub(crate) fn read_result(is_error: bool) -> LoopEvent {
    let mut output = ToolOutput::text("x");
    output.is_error = is_error;
    LoopEvent::ToolResult {
        call: call("Read", json!({"path": "src/a.rs", "limit": 5})),
        message: ToolResultMessage {
            tool_call_id: "c".into(),
            tool_name: "Read".into(),
            content: "x".into(),
            is_error,
            media: vec![],
        },
        output,
        duration_ms: 3,
    }
}

fn render(json: bool, evs: &[LoopEvent]) -> (String, String) {
    let (mut out, mut err) = (Vec::new(), Vec::new());
    {
        let mut r = Renderer::new(&mut out, &mut err, json, false);
        evs.iter().for_each(|e| r.event(e));
    }
    (
        String::from_utf8(out).unwrap(),
        String::from_utf8(err).unwrap(),
    )
}

#[test]
fn the_answer_goes_to_stdout_and_the_work_to_stderr() {
    let hello = LoopEvent::TextDelta {
        text: "Hello".into(),
    };
    let (out, err) = render(false, &[hello, read_result(false)]);
    assert_eq!(out, "Hello\n");
    assert!(err.contains("Read src/a.rs"));
}

fn turn_end(stop_reason: StopReason, error: Option<&str>) -> LoopEvent {
    LoopEvent::TurnEnd {
        stop_reason,
        usage: Usage::default(),
        duration_ms: 1,
        error: error.map(Into::into),
    }
}

/// A failed turn says why, once: a failed model call through its message,
/// anything else (a hook's refusal, a turn that could not start) at its end.
#[test]
fn a_failed_turn_shows_its_error_once() {
    let (_, err) = render(false, &[turn_end(StopReason::Error, Some("over budget"))]);
    assert!(err.contains("over budget"), "{err}");

    let mut failed = AssistantMessage::start("p", "m");
    failed.stop_reason = StopReason::Error;
    failed.error_message = Some("503 from the provider".into());
    let evs = [
        LoopEvent::Assistant { message: failed },
        turn_end(StopReason::Error, Some("503 from the provider")),
    ];
    let (_, err) = render(false, &evs);
    assert_eq!(err.matches("503 from the provider").count(), 1, "{err}");
}

#[test]
fn json_mode_writes_one_parseable_line_per_event() {
    let (out, err) = render(
        true,
        &[
            LoopEvent::TextDelta { text: "a".into() },
            LoopEvent::Guard { text: "g".into() },
        ],
    );
    let lines: Vec<Value> = out
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(lines.len(), 2);
    assert_eq!(lines[0]["type"], "text_delta");
    assert!(err.is_empty());
}

/// Every `--json` line is in the wire's envelope, one stream numbered from 1,
/// and keeps every field it had before: a reader of the old lines still works.
#[test]
fn every_json_line_carries_the_envelope_and_keeps_its_old_fields() {
    let (mut out, mut err) = (Vec::new(), Vec::new());
    {
        let mut r = Renderer::new(&mut out, &mut err, true, false);
        r.event(&LoopEvent::TextDelta { text: "a".into() });
        r.notice(Level::Warning, "budget at half");
        r.ask("Allow Bash?");
        r.event(&turn_end(StopReason::EndTurn, None));
    }
    let lines: Vec<Value> = String::from_utf8(out)
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(lines.len(), 4);
    let sid = lines[0]["sid"].as_str().unwrap().to_string();
    assert!(!sid.is_empty());
    for (n, line) in lines.iter().enumerate() {
        assert_eq!(line["v"], haest::wire::WIRE_V, "{line}");
        assert_eq!(line["sid"], sid.as_str(), "{line}");
        assert_eq!(line["seq"], n as u64 + 1, "{line}");
        assert!(line["ts"].as_u64().is_some_and(|t| t > 0), "{line}");
        assert_eq!(line["path"], json!(["main"]), "{line}");
    }
    assert_eq!(
        (&lines[0]["type"], &lines[0]["text"]),
        (&json!("text_delta"), &json!("a"))
    );
    assert_eq!(
        (&lines[1]["type"], &lines[1]["level"], &lines[1]["text"]),
        (
            &json!("notice"),
            &json!("warning"),
            &json!("budget at half")
        )
    );
    assert_eq!(
        (&lines[2]["type"], &lines[2]["text"]),
        (&json!("ask"), &json!("Allow Bash?"))
    );
    assert_eq!(lines[3]["stopReason"], "end_turn");
    // A Rust reader gets the event back from its line.
    let back: haest::wire::Envelope<LoopEvent> = serde_json::from_value(lines[3].clone()).unwrap();
    assert!(matches!(back.body, LoopEvent::TurnEnd { .. }));
    assert!(err.is_empty());
}

#[test]
fn a_notice_is_a_json_line_in_json_mode_and_stderr_otherwise() {
    let (mut out, mut err) = (Vec::new(), Vec::new());
    Renderer::new(&mut out, &mut err, true, false).notice(Level::Warning, "budget at half");
    let line: Value = serde_json::from_slice(&out).unwrap();
    assert_eq!(
        (line["type"].as_str(), line["level"].as_str()),
        (Some("notice"), Some("warning"))
    );
    assert!(err.is_empty());
    let (mut out, mut err) = (Vec::new(), Vec::new());
    Renderer::new(&mut out, &mut err, false, false).notice(Level::Warning, "budget at half");
    assert!(out.is_empty());
    assert_eq!(String::from_utf8(err).unwrap(), "budget at half\n");
}

#[test]
fn while_the_prompt_is_open_a_notice_goes_above_it_and_after_it_back_to_stderr() {
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let (tx, rx) = std::sync::mpsc::channel();
    {
        let mut r = Renderer::new(&mut out, &mut err, false, false);
        r.set_above(Some(tx));
        assert!(r.prompting());
        r.notice(Level::Info, "job b1 exited");
        r.set_above(None);
        assert!(!r.prompting());
        r.notice(Level::Info, "after");
    }
    assert_eq!(rx.try_iter().collect::<Vec<_>>(), ["job b1 exited"]);
    assert_eq!(String::from_utf8(err).unwrap(), "after\n");
    assert!(out.is_empty());
}

#[test]
fn a_notice_falls_back_to_stderr_when_the_printer_is_gone() {
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let (tx, rx) = std::sync::mpsc::channel::<String>();
    drop(rx);
    let mut r = Renderer::new(&mut out, &mut err, false, false);
    r.set_above(Some(tx));
    r.notice(Level::Info, "still heard");
    drop(r);
    assert_eq!(String::from_utf8(err).unwrap(), "still heard\n");
}

#[test]
fn a_summary_picks_the_input_that_says_what_happened() {
    assert_eq!(
        call_summary(&call(
            "Bash",
            json!({"timeout": 5, "command": "ls -la\nrm x"})
        )),
        "Bash ls -la"
    );
    assert_eq!(call_summary(&call("Thing", json!({"n": 1}))), "Thing");
}
