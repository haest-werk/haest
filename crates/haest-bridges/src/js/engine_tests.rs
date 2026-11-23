//! A Code run and a mod file are held to one caps type on one engine.
//! haest-bridges links no QuickJS of its own: a mod file's runtime can only be
//! haest-code's `JsEngine`, the one `run_in_vm` (the Code tool's) runs on, so
//! its memory cap and its interrupt watch are the engine's for both. These
//! tests hand the same `VmCaps` to each and see each cap stop each, and a
//! roomier cap let the same code through.

use std::{
    path::Path,
    time::{Duration, Instant},
};

use haest_code::{VmSpec, VmStop, run_in_vm};
use haest_core::workspace::WorkspaceMode;
use haest_mods::Mod;
use tokio_util::sync::CancellationToken;

use super::JsMod;
use crate::{VmCaps, testkit::*};

/// 8 MiB of heap; 300 ms for a run, and for a mod file's stretch of code.
fn tight() -> VmCaps {
    VmCaps::mod_file()
        .with_memory_bytes(8 << 20)
        .with_timeout(Duration::from_millis(300))
        .with_slice(Duration::from_millis(300))
}

/// Holds 16 MiB of strings at once, then says how many.
const FILL: &str = "const a = []; for (let i = 0; i < 256; i++) a.push('x'.repeat(1 << 16) + i); return String(a.length)";

fn stopped_in_time(started: Instant) -> bool {
    let took = started.elapsed();
    took >= Duration::from_millis(300) && took < Duration::from_secs(5)
}

#[tokio::test]
async fn a_code_run_is_held_to_its_caps_by_the_engine() {
    let cancel = CancellationToken::new();
    let tight_spec = VmSpec::new().with_caps(tight());
    let run = run_in_vm(FILL, &tight_spec, &cancel, |_| {}).await;
    assert!(run.error.is_some(), "{run:?}");
    assert_eq!(run.stopped, None);
    let roomy = VmSpec::new().with_caps(VmCaps::default());
    let run = run_in_vm(FILL, &roomy, &cancel, |_| {}).await;
    assert_eq!(run.value.as_deref(), Some("256"), "{run:?}");

    let started = Instant::now();
    let run = run_in_vm("for (;;) {}", &tight_spec, &cancel, |_| {}).await;
    assert_eq!(run.stopped, Some(VmStop::Timeout));
    assert!(stopped_in_time(started));
}

#[tokio::test]
async fn a_mod_file_is_held_to_its_caps_by_the_engine() {
    let source = format!(
        r#"export default function (haest) {{
          haest.on('prompt.submit', text => {{
            if (text === 'fill') {{ {FILL} }}
            if (text === 'spin') for (;;) {{}}
            return text
          }})
        }}"#
    );
    let haest = env(TestUi::new(), WorkspaceMode::Sandbox);
    let load = |caps| JsMod::load_with(Path::new("caps.js"), source.clone(), &haest, caps);
    let tight_mod = load(tight()).await.expect("the mod loads");
    assert!(tight_mod.prompt_submit("fill", &haest).await.is_err());
    let roomy = load(VmCaps::mod_file()).await.expect("the mod loads");
    let filled = roomy.prompt_submit("fill", &haest).await.unwrap();
    assert_eq!(filled.as_deref(), Some("256"));

    let started = Instant::now();
    let error = tight_mod.prompt_submit("spin", &haest).await.unwrap_err();
    assert!(error.to_string().contains("without pausing"), "{error}");
    assert!(stopped_in_time(started));
    // Each cap stopped one call: the engine still runs the next.
    let ok = tight_mod.prompt_submit("ok", &haest).await.unwrap();
    assert_eq!(ok.as_deref(), Some("ok"));
}
