#![allow(
    dead_code,
    reason = "every test binary includes this module and uses a different part of it"
)]

use std::{
    path::{Path, PathBuf},
    time::Duration,
};

use haest_core::workspace::ExecOptions;

pub(crate) mod spelling;

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
