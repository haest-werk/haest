#![allow(
    dead_code,
    reason = "every test binary includes this module and uses a different part of it"
)]

use std::{
    future,
    path::{Path, PathBuf},
    pin::pin,
    task::Poll,
    time::Duration,
};

use haest_core::workspace::{ChangeSetRef, ExecOptions};

pub(crate) mod disk;
pub(crate) mod project;

/// A temp dir by its real path: on macOS the temp root is a symlink, and the
/// sandbox mounts real paths.
pub(crate) fn tempdir() -> (tempfile::TempDir, PathBuf) {
    let t = tempfile::tempdir().unwrap();
    let real = std::fs::canonicalize(t.path()).unwrap();
    (t, real)
}

pub(crate) fn opts(cwd: &Path) -> ExecOptions {
    ExecOptions::new(cwd, Duration::from_secs(10))
}

pub(crate) fn argv(words: &[&str]) -> Vec<String> {
    words.iter().map(|w| w.to_string()).collect()
}

/// The test process's own environment, handed on as a host would.
pub(crate) fn host_env() -> haest_core::ChildEnv {
    std::env::vars().collect()
}

/// The real workspace with the test process's environment.
pub(crate) fn real() -> haest_workspace::RealWorkspace {
    haest_workspace::RealWorkspace::new(host_env())
}

/// The default sandbox over `dir`.
pub(crate) async fn sandbox(dir: &Path) -> haest_workspace::SandboxWorkspace {
    let mounts = haest_workspace::SandboxMounts::new(dir);
    haest_workspace::SandboxWorkspace::open(mounts)
        .await
        .unwrap()
}

/// Start applying `set`, and drop the apply as soon as `partway` holds, as
/// the haest command drops `/apply` 2 s after Ctrl+C. `partway` is asked
/// each time the apply wakes, before it goes on, so the drop comes at the
/// first step after which it holds. Panics if the apply ends first.
pub(crate) async fn drop_apply_partway(set: &ChangeSetRef, partway: impl Fn() -> bool) {
    let mut apply = pin!(set.apply());
    let reached = future::poll_fn(|_| {
        if partway() {
            Poll::Ready(())
        } else {
            Poll::Pending
        }
    });
    let dropped = tokio::select! {
        biased;
        () = reached => true,
        _ = &mut apply => false,
    };
    assert!(dropped, "the apply ended before it could be dropped");
}
