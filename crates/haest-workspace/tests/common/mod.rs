#![allow(dead_code)]

use std::{
    path::{Path, PathBuf},
    time::Duration,
};

use haest_core::workspace::ExecOptions;
use tokio_util::sync::CancellationToken;

/// A temp dir by its real path: on macOS the temp root is a symlink, and the
/// sandbox mounts real paths.
pub fn tempdir() -> (tempfile::TempDir, PathBuf) {
    let t = tempfile::tempdir().unwrap();
    let real = std::fs::canonicalize(t.path()).unwrap();
    (t, real)
}

pub fn opts(cwd: &Path) -> ExecOptions {
    ExecOptions {
        cwd: cwd.to_path_buf(),
        cancel: CancellationToken::new(),
        timeout: Duration::from_secs(10),
    }
}

pub fn argv(words: &[&str]) -> Vec<String> {
    words.iter().map(|w| w.to_string()).collect()
}

/// The test process's own environment, handed on as a host would.
pub fn host_env() -> haest_core::ChildEnv {
    std::env::vars().collect()
}

/// The real workspace with the test process's environment.
pub fn real() -> haest_workspace::RealWorkspace {
    haest_workspace::RealWorkspace::new(host_env())
}

/// The default sandbox over `dir`.
pub async fn sandbox(dir: &Path) -> haest_workspace::SandboxWorkspace {
    let mounts = haest_workspace::SandboxMounts::new(dir);
    haest_workspace::SandboxWorkspace::open(mounts)
        .await
        .unwrap()
}
