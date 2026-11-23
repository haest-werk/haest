//! A Code run and a mod file are held to one caps type, `VmCaps`. These tests
//! hand the same caps to each and see each cap stop each, and roomier caps
//! let the same code through. They show behaviour only. That both run on
//! haest-code's `JsEngine` rests on the build: haest-bridges has no QuickJS
//! dependency of its own, and haest-code's `__private` hands it no `Runtime`
//! or `Context` to make a second engine with.

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

/// 8 MiB of heap, and a minute for a run and for a mod file's stretch of
/// code: only the heap stops [`FILL`], however loaded the machine.
fn small_heap() -> VmCaps {
    VmCaps::mod_file()
        .with_memory_bytes(8 << 20)
        .with_timeout(Duration::from_secs(60))
        .with_slice(Duration::from_secs(60))
}

/// 300 ms for a run, and for a mod file's stretch of code.
fn short_clock() -> VmCaps {
    VmCaps::mod_file()
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
    let small = VmSpec::new().with_caps(small_heap());
    let run = run_in_vm(FILL, &small, &cancel, |_| {}).await;
    let error = run.error.clone().unwrap_or_default();
    assert!(error.contains("out of memory"), "{run:?}");
    assert_eq!(run.stopped, None);
    let roomy = VmSpec::new().with_caps(VmCaps::default());
    let run = run_in_vm(FILL, &roomy, &cancel, |_| {}).await;
    assert_eq!(run.value.as_deref(), Some("256"), "{run:?}");

    let short = VmSpec::new().with_caps(short_clock());
    let started = Instant::now();
    let run = run_in_vm("for (;;) {}", &short, &cancel, |_| {}).await;
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
    let small = load(small_heap()).await.expect("the mod loads");
    let error = small.prompt_submit("fill", &haest).await.unwrap_err();
    assert!(error.to_string().contains("out of memory"), "{error}");
    let roomy = load(VmCaps::mod_file()).await.expect("the mod loads");
    let filled = roomy.prompt_submit("fill", &haest).await.unwrap();
    assert_eq!(filled.as_deref(), Some("256"));

    let short = load(short_clock()).await.expect("the mod loads");
    let started = Instant::now();
    let error = short.prompt_submit("spin", &haest).await.unwrap_err();
    assert!(error.to_string().contains("without pausing"), "{error}");
    assert!(stopped_in_time(started));
    // Each cap stopped one call: the engine still runs the next.
    for stopped in [&small, &short] {
        let ok = stopped.prompt_submit("ok", &haest).await.unwrap();
        assert_eq!(ok.as_deref(), Some("ok"));
    }
}
