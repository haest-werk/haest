//! `haest --stdio` driven the way a parent process drives it: request lines
//! on its stdin, frame lines on its stdout, against a model on a local socket.

mod support;

use std::{process::Child, time::Duration};

use serde_json::{Value, json};
use support::{FakeModel, Home, Parent, Reply, bash, stderr, ty};

fn result(reply: &Value) -> &Value {
    assert_eq!(reply["ok"], true, "{reply}");
    &reply["result"]
}

#[test]
fn a_parent_drives_turns_stops_one_mid_tool_and_answers_every_ask() {
    let home = Home::new("stdio-drive");
    std::fs::create_dir_all(home.0.join("w/build")).unwrap();
    let model = FakeModel::new([
        // t1: an answer.
        Reply::Text("hello".into()),
        // t2: a tool that runs until it is stopped.
        bash("sleep 30"),
        // t3: an ask allowed, one allowed always, the same again (no ask),
        // another denied.
        bash("rm -rf build"),
        bash("rm -rf build"),
        bash("rm -rf build"),
        bash("rm -rf dist"),
        Reply::Text("cleaned".into()),
        // t4: an ask the interrupt withdraws.
        bash("rm -rf out"),
    ]);
    home.serve(&model);
    let mut parent = Parent::start(&home, &["--workspace", "real", "--tools", "bash"]);

    let hello = parent.until(|f| ty(f) == "hello").pop().unwrap();
    assert_eq!(hello["access"], "command");
    assert_eq!(hello["model"], "local:m");

    // Two turns, each replied at its end with how it ended.
    parent.turn("t1", "say hello");
    let frames = parent.reply("t1");
    assert_eq!(result(frames.last().unwrap())["stopReason"], "end_turn");
    let said = frames.iter().find(|f| ty(f) == "assistant").unwrap();
    assert_eq!(said["message"]["content"][0]["text"], "hello");

    parent.turn("t2", "wait a while");
    parent.until(|f| ty(f) == "assistant");
    std::thread::sleep(Duration::from_millis(300));
    parent.control("i1", "interrupt");
    let frames = parent.reply("t2");
    assert_eq!(
        result(frames.last().unwrap())["stopReason"],
        "aborted",
        "{frames:#?}"
    );
    assert!(
        frames
            .iter()
            .any(|f| ty(f) == "reply" && f["id"] == "i1" && f["ok"] == true)
    );
    assert_eq!(model.requests().len(), 2, "no model call after the stop");

    // Asks: allow, always, the same call again with no ask, deny.
    parent.turn("t3", "clean up");
    let ask = parent.ask();
    assert_eq!(ask["toolName"], "Bash");
    assert_eq!(ask["key"], "rm -rf build");
    parent.answer("a1", &ask["ask"], "allow");
    assert_eq!(parent.reply("a1").last().unwrap()["ok"], true);

    let again = parent.ask();
    assert_ne!(again["ask"], ask["ask"], "an allow covers one call");
    parent.answer("a2", &again["ask"], "always");

    let frames = parent.until(|f| ty(f) == "approval_ask");
    let allowed = frames
        .iter()
        .filter(|f| ty(f) == "notice")
        .map(|f| f["text"].as_str().unwrap());
    assert!(
        allowed
            .clone()
            .any(|t| t.starts_with("Allowed Bash (always)")),
        "the third call ran on the always: {frames:#?}"
    );
    let other = frames.last().unwrap();
    assert_eq!(
        other["key"], "rm -rf dist",
        "always covers only its own command"
    );
    parent.answer("a3", &other["ask"], "deny");

    let frames = parent.reply("t3");
    assert_eq!(result(frames.last().unwrap())["stopReason"], "end_turn");
    let denied = frames.iter().rfind(|f| ty(f) == "tool_result").unwrap();
    assert_eq!(denied["message"]["isError"], true, "{denied}");
    assert!(!home.0.join("w/build").exists(), "the allowed calls ran");

    // An ask the interrupt withdraws: an answer after it is refused.
    parent.turn("t4", "clean more");
    let ask = parent.ask();
    parent.control("i2", "interrupt");
    let frames = parent.reply("t4");
    assert_eq!(
        result(frames.last().unwrap())["stopReason"],
        "aborted",
        "{frames:#?}"
    );
    let withdrawn = frames
        .iter()
        .find(|f| ty(f) == "approval_withdrawn")
        .expect("withdrawn");
    assert_eq!(withdrawn["ask"], ask["ask"]);
    parent.answer("a4", &ask["ask"], "allow");
    let late = parent.reply("a4").pop().unwrap();
    assert_eq!(late["ok"], false, "{late}");

    // A request of another wire version is refused with its id.
    parent.send(json!({"v": 2, "id": "v2", "type": "turn", "text": "hi"}));
    let refused = parent.reply("v2").pop().unwrap();
    assert_eq!(refused["ok"], false);
    assert!(
        refused["error"].as_str().unwrap().contains("version"),
        "{refused}"
    );

    // Stdin closes: haest exits, its session saved.
    let (code, err) = parent.finish();
    assert_eq!(code, Some(0), "{err}");
    assert_eq!(home.sessions().len(), 1);
    assert_eq!(
        model.requests().len(),
        8,
        "every reply of the script was asked for"
    );
}

#[test]
fn a_close_ends_the_run_while_the_parent_still_holds_stdin() {
    let home = Home::new("stdio-close");
    let model = FakeModel::new([]);
    home.serve(&model);
    let mut parent = Parent::start(&home, &[]);
    parent.until(|f| ty(f) == "hello");

    parent.control("x", "close");
    assert_eq!(parent.reply("x").last().unwrap()["ok"], true);
    // Stdin is still open: haest exits on its own.
    let status = wait(&mut parent.child, Duration::from_secs(10));
    assert_eq!(status, Some(0));
}

#[test]
fn stdio_takes_no_prompt() {
    let home = Home::new("stdio-prompt");
    let out = home.haest(&["--stdio", "do it"], &[]);
    assert_eq!(out.status.code(), Some(2));
    assert!(
        stderr(&out).contains("--stdio takes its requests on stdin"),
        "{}",
        stderr(&out)
    );
}

/// The child's exit code, once it has exited within `limit`.
fn wait(child: &mut Child, limit: Duration) -> Option<i32> {
    let start = std::time::Instant::now();
    while start.elapsed() < limit {
        if let Some(status) = child.try_wait().unwrap() {
            return status.code();
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let _ = child.kill();
    panic!("still running after {limit:?}");
}
