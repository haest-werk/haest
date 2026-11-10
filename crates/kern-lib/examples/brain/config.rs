//! brain's kern settings: its own TOML text, read through `kern::settings`.
//! No `KERN_*` variable and no flag reaches a run: the node's environment must
//! not change what a person's brain can do. Bare is the start; this file turns
//! on what brain uses, and a profile per kind of run turns it back off.
//!
//! PLUG POINT: `settings::settle` (inputs a host already holds, no file reads).

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};

use kern::{
    limits::TurnLimits,
    settings::{Inputs, Settings, settle},
};

use crate::CWD;

pub const BRAIN_TOML: &str = r#"
# A work run: kern's file tools and its bash. Glob and Grep are brain's own,
# on the node; WebFetch is the node's too.
tools = ["read", "write", "edit", "bash"]
agents = true
max_turn_minutes = 1

[profiles.dream]
# A dream reads what the node hands it: nothing that writes, no agents. Read
# and Bash stay, over a shut workspace: they reach no file and no shell.
tools = ["read", "bash"]
agents = false
"#;

/// brain's settings for a run, with a profile (`dream`) or without.
pub fn settings(profile: Option<&str>) -> Settings {
    let flags: Vec<(String, String)> = profile
        .map(|p| ("profile".to_string(), p.to_string()))
        .into_iter()
        .collect();
    let inputs = Inputs {
        cwd: Path::new(CWD),
        home: Path::new("/node"),
        env: &HashMap::new(),
        flags: &flags,
        user_file: Some((PathBuf::from("brain.toml"), BRAIN_TOML.into())),
        project_file: None,
    };
    settle(&inputs).expect("brain.toml checks").settings
}

/// The turn caps a run's host hands the loop (the gateway's models are not in
/// kern's catalog: the default window).
pub fn limits(s: &Settings) -> TurnLimits {
    s.limits_for(s.window(None))
}
