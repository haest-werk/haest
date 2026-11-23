//! The `hooks` setting read and written: both kinds, both dialects, every
//! entry a config file may not hold refused naming the entry and never a
//! header's value, the same entries built in Rust and held to the same
//! rules, `${NAME}` in a header filled in, and what Debug hides.

use std::time::Duration;

use serde_json::{Value, json};

use super::*;

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
    assert_eq!(s.url_entries(), ["[[hooks.\"turn.end\"]] #1"]);

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

/// A matcher is one tool's name or one prefix: Claude Code's `Bash.*` and a
/// list (`Edit,Write`, `Edit Write`) match no tool, so a guard bound with
/// one never fires. Each is refused saying which it is.
#[test]
fn a_matcher_that_is_a_regular_expression_or_a_list_says_which() {
    let why = |matcher: &str| {
        read(json!({ "tool.before": [{ "command": "x", "matcher": matcher }] })).unwrap_err()
    };
    let regex = why("Bash.*");
    assert!(
        regex.contains("`matcher = \"Bash.*\"` is not haest's pattern")
            && regex.contains("Claude Code's regular expressions are not read"),
        "{regex}"
    );
    for list in ["Edit,Write", "Edit Write", "Edit, Write"] {
        let refused = why(list);
        assert!(
            refused.contains("is a list") && refused.contains("`,` or a space"),
            "{refused}"
        );
    }
}

/// A `timeout` too long for a `Duration` is refused as too long, not as a
/// number below 0.
#[test]
fn a_timeout_too_long_is_refused_as_too_long() {
    let why = read(json!({ "turn.end": [{ "command": "x", "timeout": 1e300 }] })).unwrap_err();
    assert!(
        why.contains("`timeout`: 1e300 seconds is too long"),
        "{why}"
    );
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
            "[[hooks.\"turn.end\"]] #1: header x-missing names ${NOPE}, which is not set and has no default: sent as written"
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

/// A Rust host builds what a file holds, entry by entry, and is refused
/// what a file is refused, in the same words.
#[test]
fn entries_built_in_rust_are_the_files_and_held_to_its_rules() {
    let built = HookSettings::default()
        .with_entry(
            ModEvent::TurnEnd,
            HookEntry::http(" https://review.example/turn ")
                .with_header("authorization", "Bearer t")
                .with_timeout(Duration::from_millis(2_500)),
        )
        .and_then(|s| {
            s.with_entry(
                ModEvent::ToolBefore,
                HookEntry::shell("sh guard.sh")
                    .with_dialect(HookDialect::Claude)
                    .with_matcher("Bash")
                    .with_timeout(Duration::from_secs(10))
                    .with_on_error(OnError::Continue),
            )
        })
        .and_then(|s| s.with_entry(ModEvent::ToolBefore, HookEntry::shell("./log.sh")))
        .unwrap();
    let read = read(json!({
        "turn.end": [{ "url": "https://review.example/turn", "headers": { "authorization": "Bearer t" }, "timeout": 2.5 }],
        "tool.before": [
            { "command": "sh guard.sh", "dialect": "claude", "matcher": "Bash", "timeout": 10, "on_error": "continue" },
            { "command": "./log.sh" },
        ],
    }))
    .unwrap();
    assert_eq!(
        built, read,
        "events in the protocol's order, as a file has them"
    );
    let names: Vec<String> = built.named_entries().map(|(_, name, _)| name).collect();
    assert_eq!(
        names,
        [
            "[[hooks.\"tool.before\"]] #1",
            "[[hooks.\"tool.before\"]] #2",
            "[[hooks.\"turn.end\"]] #1"
        ]
    );

    let refused = [
        (
            ModEvent::TurnEnd,
            HookEntry::shell(" "),
            "`command` is empty",
        ),
        (
            ModEvent::TurnEnd,
            HookEntry::http(format!("ftp://u:{TOKEN}@a.example")),
            "`url` must start with http:// or https://",
        ),
        (
            ModEvent::ModelBefore,
            HookEntry::shell("x").with_dialect(HookDialect::Claude),
            "model.before has no Claude Code event",
        ),
        (
            ModEvent::TurnEnd,
            HookEntry::shell("x").with_matcher("Bash"),
            "`matcher` picks tools, on tool.before and tool.after only",
        ),
        (
            ModEvent::ToolBefore,
            HookEntry::shell("x").with_matcher("Edit|Write"),
            "Claude Code's regular expressions are not read",
        ),
        (
            ModEvent::TurnEnd,
            HookEntry::shell("x").with_timeout(Duration::ZERO),
            "`timeout` must be a number of seconds above 0",
        ),
        (
            ModEvent::TurnEnd,
            HookEntry::shell("x").with_header("authorization", TOKEN),
            "`headers` are for a `url` hook only",
        ),
        (
            ModEvent::TurnEnd,
            HookEntry::http("https://a.example").with_header("bad name", TOKEN),
            "`bad name` is not a header name",
        ),
    ];
    for (event, entry, says) in refused {
        let why = HookSettings::default()
            .with_entry(event, entry)
            .unwrap_err();
        assert_eq!(why.event(), event);
        let why = why.to_string();
        assert!(why.starts_with(&format!("a {event} hook: ")), "{why}");
        assert!(why.contains(says) && !why.contains(TOKEN), "{why}");
    }
}
