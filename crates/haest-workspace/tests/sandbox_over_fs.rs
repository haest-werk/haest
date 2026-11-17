//! A host's own file system under the sandbox shell (`SandboxWorkspace::over`):
//! what the host says when it refuses reaches the model through the shell.

mod common;

use std::{io, path::Path, sync::Arc};

use async_trait::async_trait;
use common::opts;
use haest_core::{
    Workspace,
    fs::{DirEntry, Fs, FsStat, MemFs},
};
use haest_workspace::SandboxWorkspace;

const WHY: &str = "EACCES: permission denied, open '/locked/a.md' (the person asking can't edit there; tell them instead)";

/// MemFs, but `/locked` is read-only, with the host's reason.
struct Locked(MemFs);

fn deny(path: &Path) -> io::Result<()> {
    if path.starts_with("/locked") {
        return Err(io::Error::new(io::ErrorKind::PermissionDenied, WHY));
    }
    Ok(())
}

#[async_trait]
impl Fs for Locked {
    async fn read(&self, path: &Path) -> io::Result<Vec<u8>> {
        self.0.read(path).await
    }
    async fn write(&self, path: &Path, data: &[u8]) -> io::Result<()> {
        deny(path)?;
        self.0.write(path, data).await
    }
    async fn append(&self, path: &Path, data: &[u8]) -> io::Result<()> {
        deny(path)?;
        self.0.append(path, data).await
    }
    async fn stat(&self, path: &Path) -> io::Result<FsStat> {
        self.0.stat(path).await
    }
    async fn read_dir(&self, path: &Path) -> io::Result<Vec<DirEntry>> {
        self.0.read_dir(path).await
    }
    async fn create_dir(&self, path: &Path) -> io::Result<()> {
        self.0.create_dir(path).await
    }
    async fn create_dir_all(&self, path: &Path) -> io::Result<()> {
        self.0.create_dir_all(path).await
    }
    async fn remove(&self, path: &Path) -> io::Result<()> {
        deny(path)?;
        self.0.remove(path).await
    }
    async fn remove_all(&self, path: &Path) -> io::Result<()> {
        deny(path)?;
        self.0.remove_all(path).await
    }
    async fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
        deny(from)?;
        deny(to)?;
        self.0.rename(from, to).await
    }
}

/// `mv a b` replaces `b` with `a`, mode and all, as POSIX rename does. Over a
/// host's `Fs`, `b` kept its own remembered mode, so a plain file moved over
/// a script read as executable.
#[tokio::test]
async fn a_moved_file_takes_its_own_mode_over_the_one_it_replaced() {
    let ws = SandboxWorkspace::over(MemFs::new());
    let r = ws
        .shell(
            "printf 'plain\\n' > a; printf 'echo hi\\n' > b; chmod +x b; mv a b; \
             test -x b && echo b-runs; \
             printf 'echo hi\\n' > c; chmod +x c; printf 'plain\\n' > d; mv c d; \
             test -x d && echo d-runs",
            opts(Path::new("/")),
        )
        .await;
    assert_eq!(r.stdout, "d-runs\n", "{r:?}");
}

#[tokio::test]
async fn a_hosts_reason_for_refusing_a_write_reaches_the_shells_output() {
    let mem = MemFs::default();
    mem.create_dir_all(Path::new("/locked")).await.unwrap();
    let ws = SandboxWorkspace::over(Arc::new(Locked(mem)));
    let r = ws
        .shell(
            "echo hi > /locked/a.md; echo \"exit $?\"",
            opts(Path::new("/")),
        )
        .await;
    assert!(r.stderr.contains(WHY), "the host's reason, whole: {r:?}");
    assert_eq!(r.stdout, "exit 1\n");
    // A plain refusal (no reason of the host's) still reads like a shell's.
    let r = ws
        .shell("rm /locked/a.md 2>&1; true", opts(Path::new("/")))
        .await;
    assert!(!r.stdout.contains("os error"), "{r:?}");
}
