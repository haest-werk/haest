//! Turns as the `haest` binary runs them, against a model on a local socket:
//! the session runs the turn, and the terminal shows its stream.

mod support;

use serde_json::Value;
use support::{FakeModel, Home, LOCAL_KEY, Reply, stderr};

#[test]
fn print_mode_prints_the_answer_and_the_cost_line_after_it() {
    let home = Home::new("print-turn");
    let model = FakeModel::new([Reply::Text("hello there".into())]);
    home.serve(&model);

    let out = home.haest(&["-p", "say hello", "-m", "local:m"], &[LOCAL_KEY]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert_eq!(String::from_utf8_lossy(&out.stdout), "hello there\n");
    assert!(
        stderr(&out).contains("10 in / 2 out tokens"),
        "{}",
        stderr(&out)
    );
    let sent = model.requests();
    assert_eq!(sent.len(), 1);
    let last = &sent[0]["messages"].as_array().unwrap().last().unwrap()["content"];
    assert!(last.to_string().contains("say hello"), "{last}");
    assert_eq!(home.sessions().len(), 1, "the session was saved");
}

#[test]
fn json_mode_is_one_numbered_stream_of_the_whole_run() {
    let home = Home::new("json-turn");
    let model = FakeModel::new([Reply::Text("hi".into())]);
    home.serve(&model);

    let out = home.haest(&["--json", "-p", "say hi", "-m", "local:m"], &[LOCAL_KEY]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    let lines: Vec<Value> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    let sid = lines[0]["sid"].as_str().unwrap();
    for (n, line) in lines.iter().enumerate() {
        assert_eq!(line["v"], 1, "{line}");
        assert_eq!(line["sid"], sid, "{line}");
        assert_eq!(
            line["seq"],
            n as u64 + 1,
            "one stream, numbered from 1: {line}"
        );
    }
    let types: Vec<&str> = lines.iter().map(|l| l["type"].as_str().unwrap()).collect();
    let at = |t: &str| {
        types
            .iter()
            .position(|x| *x == t)
            .unwrap_or_else(|| panic!("no {t}: {types:?}"))
    };
    assert!(
        at("user") < at("text_delta") && at("text_delta") < at("turn_end"),
        "{types:?}"
    );
    let states: Vec<&str> = (lines.iter())
        .filter(|l| l["type"] == "status")
        .map(|l| l["state"].as_str().unwrap())
        .collect();
    assert_eq!(states.first(), Some(&"idle"));
    assert!(states.contains(&"running"), "{states:?}");
    assert_eq!(states.last(), Some(&"idle"), "{states:?}");
    assert!(stderr(&out).is_empty(), "{}", stderr(&out));
}
