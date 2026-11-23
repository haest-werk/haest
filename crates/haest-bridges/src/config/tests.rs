//! The `hooks` setting as this crate binds it: the events its entries may
//! name are the ones the bridges take, and every entry binds (bound.rs).

use haest_mods::{HookSettings, ModEvent};
use serde_json::{Value, json};

mod bound;

fn read(setting: Value) -> Result<HookSettings, String> {
    serde_json::from_value(setting).map_err(|e| e.to_string())
}

/// The two lists that say which events take what hold what the hooks do:
/// a `Claude` hook binds exactly the events Claude Code has (the shell and
/// HTTP tests' map), and a matcher is for the events about a tool.
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
