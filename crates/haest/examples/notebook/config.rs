//! The notebook's haest settings, written out. No file, no `HAEST_*` variable
//! and no flag reaches a run: the node's environment must not change what a
//! person's notebook can do, and nothing in haest reads one on its own. Bare
//! is the start; a work run turns on what the notebook uses, a dream turns
//! most of it back off.
//!
//! PLUG POINT: `settings::Settings`, hardcoded (no loader).

use haest::{
    TurnCaps,
    settings::{BuiltinTools, CapSettings, Settings},
};

use crate::CWD;

/// A work run: haest's file tools and its bash. Glob and Grep are the notebook's own,
/// on the node; WebFetch is the node's too.
pub fn work() -> Settings {
    let bare = Settings::bare(CWD, "/node");
    Settings {
        tools: BuiltinTools {
            read: true,
            write: true,
            edit: true,
            bash: true,
            ..BuiltinTools::NONE
        },
        agents: true,
        caps: CapSettings {
            max_turn_minutes: 1.0,
            ..CapSettings::default()
        },
        ..bare
    }
}

/// A dream reads what the node hands it: nothing that writes, no agents. Read
/// and Bash stay, over a shut workspace: they reach no file and no shell.
pub fn dream() -> Settings {
    Settings {
        tools: BuiltinTools {
            read: true,
            bash: true,
            ..BuiltinTools::NONE
        },
        agents: false,
        ..work()
    }
}

/// The turn caps a run's host hands the loop (the gateway's models are not in
/// haest's catalog: the default window).
pub fn turn_caps(s: &Settings) -> TurnCaps {
    s.turn_caps_for(s.window(None))
}
