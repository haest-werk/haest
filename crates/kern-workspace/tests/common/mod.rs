#![allow(dead_code)]

use std::{
    path::{Path, PathBuf},
    time::Duration,
};

use kern_core::workspace::ExecOptions;
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
pub fn host_env() -> kern_core::ChildEnv {
    std::env::vars().collect()
}

/// The real workspace with the test process's environment.
pub fn real() -> kern_workspace::RealWorkspace {
    kern_workspace::RealWorkspace::new(host_env())
}
