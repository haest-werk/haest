//! The `[timeouts]` and `[limits]` settings reach what they bound in a built
//! conversation: the Code tool's caps, a mod file's load deadline.

use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use haest_core::fs::MemFs;
use haest_settings::{JsLimits, JsTimeouts, LimitSettings, Settings, TimeoutSettings};

use super::{hardcoded, keyed_with};
use crate::BuildOptions;

/// `javascript` timeouts over the defaults.
fn timeouts(js: JsTimeouts) -> TimeoutSettings {
    TimeoutSettings {
        javascript: js,
        ..TimeoutSettings::default()
    }
}

#[tokio::test]
async fn the_code_tool_of_a_built_conversation_has_the_settings_caps() {
    let s = Settings {
        code: true,
        timeouts: timeouts(JsTimeouts {
            code: Duration::from_secs(90),
            ..JsTimeouts::default()
        }),
        limits: LimitSettings {
            javascript: JsLimits {
                code_memory: 32 << 20,
                ..JsLimits::default()
            },
        },
        ..hardcoded()
    };
    let rt = keyed_with(&Arc::new(MemFs::new()), s, BuildOptions::default()).await;
    let tools = rt.agent().deps().tools().tools();
    let code = tools
        .iter()
        .find(|t| t.name() == "Code")
        .expect("code is on");
    let said = code.description();
    assert!(said.contains("32 MB, 90s"), "{said}");
}

/// A mod file whose default export never returns stops loading at
/// `mod_load`, and the human is told; the conversation goes on without it.
#[tokio::test]
async fn a_mod_file_that_never_returns_stops_loading_at_mod_load() {
    let fs = Arc::new(MemFs::from_files([(
        "/h/.config/haest/mods/stuck.js",
        "export default () => new Promise(() => {})",
    )]));
    let s = Settings {
        mods: true,
        timeouts: timeouts(JsTimeouts {
            mod_load: Duration::from_millis(300),
            ..JsTimeouts::default()
        }),
        ..hardcoded()
    };
    let started = Instant::now();
    let rt = keyed_with(&fs, s, BuildOptions::default()).await;
    assert!(started.elapsed() < Duration::from_secs(10));
    let warned = rt.ui().warnings();
    assert!(
        warned
            .iter()
            .any(|w| w.contains("stuck.js") && w.contains("did not return within 0.3s")),
        "{warned:?}"
    );
}
