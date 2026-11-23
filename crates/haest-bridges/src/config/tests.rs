//! The `hooks` setting read and written: both kinds, both dialects, every
//! entry a config file may not hold refused naming the entry and never a
//! header's value, `${NAME}` in a header filled in, and what Debug hides.

use std::time::Duration;

use haest_mods::{ModEvent, OnError};
use serde_json::{Value, json};

use super::*;

#[cfg(any(feature = "shell", feature = "http"))]
mod bound;

fn read(setting: Value) -> Result<HookSettings, String> {
    serde_json::from_value(setting).map_err(|e| e.to_string())
}

/// The secret every refusal below must keep to itself.
const TOKEN: &str = "s3cret-t0ken";

#[test]
fn a_command_and_a_url_in_either_dialect_read_with_their_options() {
    let s = read(json!({
        "turn.end": [{ "url": "https://review.example/turn", "headers": { "authorization": "Bearer t" }, "timeout": 2.5 }],
        "tool.before": [
            { "command": "sh guard.sh", "dialect": "claude", "matcher": "Bash", "timeout": 10, "on_error": "continue" },
            { "command": "./log.sh" },
        ],
    }))
    .unwrap();
    let entries: Vec<(ModEvent, &HookEntry)> = s.entries().collect();
    assert_eq!(entries.len(), 3);
    // Events in the protocol's order, each one's entries as written.
    let (event, guard) = entries[0];
    assert_eq!(event, ModEvent::ToolBefore);
    assert_eq!(guard.command(), Some("sh guard.sh"));
    assert_eq!(guard.url(), None);
    assert_eq!(guard.dialect(), HookDialect::Claude);
    assert_eq!(guard.matcher(), Some("Bash"));
    assert_eq!(guard.timeout(), Some(Duration::from_secs(10)));
    assert_eq!(guard.on_error(), Some(OnError::Continue));
    let (_, log) = entries[1];
    assert_eq!(log.command(), Some("./log.sh"));
    assert_eq!(log.dialect(), HookDialect::Haest);
    assert_eq!(
        (log.matcher(), log.timeout(), log.on_error()),
        (None, None, None)
    );
    let (event, review) = entries[2];
    assert_eq!(event, ModEvent::TurnEnd);
    assert_eq!(review.url(), Some("https://review.example/turn"));
    assert_eq!(review.timeout(), Some(Duration::from_millis(2_500)));
    assert_eq!(review.header_names().collect::<Vec<_>>(), ["authorization"]);

    // Written back as a file says it, and read again the same.
    let written = serde_json::to_value(&s).unwrap();
    assert_eq!(written["tool.before"][1], json!({ "command": "./log.sh" }));
    assert_eq!(written["turn.end"][0]["timeout"], json!(2.5));
    assert_eq!(written["tool.before"][0]["timeout"], json!(10));
    assert_eq!(read(written).unwrap(), s);
}

#[test]
fn nothing_set_and_an_empty_list_bind_nothing() {
    assert!(read(json!({})).unwrap().is_empty());
    assert_eq!(
        read(json!({ "turn.end": [] })).unwrap(),
        HookSettings::default()
    );
}

/// Each refusal names the entry as the file has it (the event, its number
/// from 1) and says what is wrong.
#[test]
fn what_an_entry_may_not_hold_is_refused_naming_the_entry() {
    let cases: [(Value, &str); 13] = [
        (
            json!({ "tool.befor": [{ "command": "x" }] }),
            "unknown event \"tool.befor\"",
        ),
        (
            json!({ "tool": { "before": [] } }),
            "quote an event's name, [[hooks.\"tool.before\"]]",
        ),
        (
            json!({ "turn.end": { "command": "x" } }),
            "\"turn.end\" must be a list of tables",
        ),
        (
            json!({ "turn.end": [{ "command": "x" }, { "comand": "y" }] }),
            "[[hooks.\"turn.end\"]] #2: unknown key `comand`",
        ),
        (
            json!({ "turn.end": [{ "command": "x", "url": "https://a.example" }] }),
            "[[hooks.\"turn.end\"]] #1: takes `command` or `url`, not both",
        ),
        (
            json!({ "turn.end": [{ "dialect": "claude" }] }),
            "#1: needs `command`",
        ),
        (
            json!({ "model.before": [{ "command": "x", "dialect": "claude" }] }),
            "[[hooks.\"model.before\"]] #1: model.before has no Claude Code event",
        ),
        (
            json!({ "model.after": [{ "url": "https://a.example", "dialect": "claude" }] }),
            "model.after has no Claude Code event",
        ),
        (
            json!({ "turn.end": [{ "command": "x", "matcher": "Bash" }] }),
            "[[hooks.\"turn.end\"]] #1: `matcher` picks tools, on tool.before and tool.after only",
        ),
        (
            json!({ "tool.before": [{ "command": "x", "matcher": "Edit|Write" }] }),
            "Claude Code's regular expressions are not read",
        ),
        (
            json!({ "turn.end": [{ "command": "x", "dialect": "codex" }] }),
            "\"codex\"",
        ),
        (
            json!({ "turn.end": [{ "command": "x", "on_error": "ignore" }] }),
            "\"ignore\"",
        ),
        (
            json!({ "turn.end": [{ "command": "x", "timeout": "10s" }] }),
            "`timeout` must be a number of seconds above 0",
        ),
    ];
    for (setting, says) in cases {
        let why = read(setting.clone()).unwrap_err();
        assert!(why.contains(says), "{setting}: {why}");
    }
    for timeout in [json!(0), json!(-1), json!(1e300)] {
        let why =
            read(json!({ "turn.end": [{ "command": "x", "timeout": timeout }] })).unwrap_err();
        assert!(why.contains("`timeout`"), "{timeout}: {why}");
    }
    for matcher in ["", "Bash.*", "*Bash", "mcp__a__*b*", "(Bash)"] {
        let setting = json!({ "tool.after": [{ "command": "x", "matcher": matcher }] });
        assert!(read(setting).is_err(), "{matcher}");
    }
    for matcher in ["Bash", "mcp__github__*", "*"] {
        let setting = json!({ "tool.after": [{ "command": "x", "matcher": matcher }] });
        assert!(read(setting).is_ok(), "{matcher}");
    }
}

/// Neither a header's value nor a URL (which may carry a password) is in a
/// refusal.
#[test]
fn a_refusal_never_holds_a_headers_value_or_the_url() {
    let url = format!("ftp://user:{TOKEN}@review.example/turn");
    let cases = [
        json!({ "turn.end": [{ "url": "https://a.example", "headers": { "authorization": 7 } }] }),
        json!({ "turn.end": [{ "url": "https://a.example", "headers": TOKEN }] }),
        json!({ "turn.end": [{ "url": "https://a.example", "headers": [TOKEN] }] }),
        json!({ "turn.end": [{ "command": "x", "headers": { "authorization": TOKEN } }] }),
        json!({ "turn.end": [{ "url": url }] }),
        json!({ "turn.end": [{ "url": "https://a.example", "headers": { "authorization": TOKEN }, "oops": 1 }] }),
        json!({ "turn.end": [{ "command": TOKEN.len() }] }),
    ];
    for setting in cases {
        let why = read(setting.clone()).unwrap_err();
        assert!(!why.contains(TOKEN) && !why.contains('7'), "{why}");
        assert!(why.contains("[[hooks.\"turn.end\"]] #1"), "{why}");
    }
}

#[test]
fn a_headers_name_in_variables_is_filled_in_and_an_unset_one_is_said() {
    let s = read(json!({
        "turn.end": [{ "url": "https://a.example", "headers": {
            "authorization": "Bearer ${REVIEW_TOKEN}",
            "x-team": "${TEAM:-core}",
            "x-missing": "${NOPE}",
        } }],
    }))
    .unwrap();
    let env = |name: &str| (name == "REVIEW_TOKEN").then(|| TOKEN.to_string());
    let (filled, notes) = s.filled(&env);
    let headers = &filled.events[0].1[0].headers;
    let value = |name: &str| {
        headers
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, v)| v.as_str())
    };
    assert_eq!(
        value("authorization"),
        Some(format!("Bearer {TOKEN}").as_str())
    );
    assert_eq!(value("x-team"), Some("core"));
    assert_eq!(value("x-missing"), Some("${NOPE}"), "left as written");
    assert_eq!(
        notes,
        [
            "[[hooks.\"turn.end\"]] #1: header x-missing names ${NOPE}, which is not set and has no default"
        ]
    );
    let written = &s.events[0].1[0].headers;
    assert!(
        written.contains(&("authorization".into(), "Bearer ${REVIEW_TOKEN}".into())),
        "the setting itself is as written"
    );
}

#[test]
fn debug_shows_a_header_by_its_name_and_a_url_without_its_credentials() {
    let s = read(json!({
        "turn.end": [{
            "url": format!("https://user:{TOKEN}@review.example/turn?key={TOKEN}"),
            "headers": { "authorization": format!("Bearer {TOKEN}") },
        }],
    }))
    .unwrap();
    let shown = format!("{s:?}");
    assert!(!shown.contains(TOKEN), "{shown}");
    assert!(
        shown.contains("authorization") && shown.contains("review.example"),
        "{shown}"
    );
}

/// The two lists that say which events take what hold what the hooks do:
/// a `Claude` hook binds exactly the events Claude Code has (the shell and
/// HTTP tests' map), and a matcher is for the events about a tool.
#[cfg(any(feature = "shell", feature = "http"))]
#[test]
fn a_claude_entry_and_a_matcher_go_on_the_events_that_take_them() {
    for event in ModEvent::ALL {
        let claude = json!({ event.as_str(): [{ "command": "x", "dialect": "claude" }] });
        let has = crate::hook_tests::claude_event(*event).is_some();
        assert_eq!(read(claude).is_ok(), has, "{event}");
        let matcher = json!({ event.as_str(): [{ "command": "x", "matcher": "Bash" }] });
        let about_a_tool = matches!(event, ModEvent::ToolBefore | ModEvent::ToolAfter);
        assert_eq!(read(matcher).is_ok(), about_a_tool, "{event}");
    }
}
