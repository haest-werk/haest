//! [`RealFs::try_lock`](super::RealFs): an exclusive `flock(2)` on a file.

use std::{io, path::Path};

use haest_core::fs::FsLock;

use super::fs::open_file;

/// An exclusive `flock(2)` on the file at `path`, held until the lock drops.
pub(super) async fn exclusive(path: &Path) -> io::Result<Option<FsLock>> {
    let file = open_to_lock(path).await?.into_std().await;
    match flock_exclusive(&file) {
        Ok(()) => Ok(Some(FsLock::new(file))),
        Err(e) if takes_no_locks(&e) => Ok(None),
        Err(e) => Err(e),
    }
}

/// `path` opened to read and write: an NFS client takes an exclusive
/// `flock` as a write lock on the server, and refuses it on a file opened
/// only to read (EBADF on Linux, EIO on macOS). Where the file or its mount
/// may not be written (a session file made 0400, a read-only mount), it is
/// opened only to read, which a local disk locks all the same.
async fn open_to_lock(path: &Path) -> io::Result<tokio::fs::File> {
    match open_file(path, true).await {
        Err(e)
            if matches!(
                e.kind(),
                io::ErrorKind::PermissionDenied | io::ErrorKind::ReadOnlyFilesystem
            ) =>
        {
            open_file(path, false).await
        }
        opened => opened,
    }
}

/// Lock `file` for this open file alone, without waiting: `WouldBlock` when
/// another open file of it (in this process or another) holds the lock.
/// `std::fs::File::try_lock` does the same from Rust 1.89, past haest's MSRV.
#[expect(
    unsafe_code,
    reason = "flock(2) takes a lock the system drops with the process; std's needs Rust 1.89"
)]
fn flock_exclusive(file: &std::fs::File) -> io::Result<()> {
    use std::os::fd::AsRawFd;
    loop {
        // SAFETY: the descriptor is `file`'s, open for the whole call.
        let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
        if rc == 0 {
            return Ok(());
        }
        let e = io::Error::last_os_error();
        if e.kind() != io::ErrorKind::Interrupted {
            return Err(e);
        }
    }
}

/// The file system takes no `flock(2)` on this file: it has none (some
/// network file systems), or refuses it on a file opened only to read (an
/// NFS client on Linux, EBADF). A run then goes on with no lock, as it did
/// before haest took one, rather than stop.
fn takes_no_locks(e: &io::Error) -> bool {
    e.kind() == io::ErrorKind::Unsupported
        || matches!(e.raw_os_error(), Some(libc::ENOLCK | libc::EBADF))
}

#[cfg(test)]
mod tests {
    use std::{
        os::unix::fs::PermissionsExt,
        path::PathBuf,
        process::{Command, Stdio},
        time::Duration,
    };

    use haest_core::fs::Fs;

    use super::*;
    use crate::RealFs;

    fn refused(lock: &io::Result<Option<FsLock>>) -> bool {
        lock.as_ref()
            .is_err_and(|e| e.kind() == io::ErrorKind::WouldBlock)
    }

    /// The lock on `file` once it is free, waiting at most 5 s. A process
    /// another test forks shares this one's open files until it starts its
    /// program, so a lock just dropped can stay held that long.
    async fn lock_once_free(file: &Path) -> io::Result<Option<FsLock>> {
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            let lock = RealFs.try_lock(file).await;
            if !refused(&lock) || std::time::Instant::now() > deadline {
                return lock;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    /// This test binary, to run the one test `test` (of this module) alone.
    fn rerun(test: &str) -> Command {
        let (_, module) = module_path!().split_once("::").unwrap();
        let mut run = Command::new(std::env::current_exe().unwrap());
        run.args([&format!("{module}::{test}"), "--exact"])
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        run
    }

    #[tokio::test]
    async fn a_locked_file_is_refused_until_its_lock_drops() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("s.jsonl");
        std::fs::write(&file, "{}\n").unwrap();

        let held = RealFs.try_lock(&file).await.unwrap();
        assert!(held.is_some(), "the disk takes locks");
        let again = RealFs.try_lock(&file).await;
        assert!(refused(&again), "held in this process");
        drop(held);
        assert!(lock_once_free(&file).await.unwrap().is_some());

        let missing = RealFs.try_lock(&dir.path().join("none")).await;
        assert!(missing.is_err_and(|e| e.kind() == io::ErrorKind::NotFound));
    }

    /// A session file the human made read-only (0400) still locks, and
    /// still keeps a second holder out.
    #[tokio::test]
    async fn a_read_only_file_still_locks() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("s.jsonl");
        std::fs::write(&file, "{}\n").unwrap();
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o400)).unwrap();

        let held = RealFs.try_lock(&file).await.unwrap();
        assert!(held.is_some(), "the disk takes locks");
        assert!(refused(&RealFs.try_lock(&file).await), "held");
    }

    /// The errors a file system gives for a lock it never takes read as no
    /// lock, so a run goes on without one rather than stop.
    #[test]
    fn a_lock_the_file_system_never_takes_is_no_lock() {
        for errno in [libc::ENOSYS, libc::ENOLCK, libc::EBADF] {
            let e = io::Error::from_raw_os_error(errno);
            assert!(takes_no_locks(&e), "errno {errno}");
        }
        assert!(!takes_no_locks(&io::Error::from_raw_os_error(
            libc::EWOULDBLOCK
        )));
    }

    /// Set in a run of this test binary that holds the file it names.
    const HOLD: &str = "HAEST_TEST_HOLD_LOCK";

    /// A process that dies holding a lock leaves the file free: a crash
    /// never leaves a session locked.
    #[tokio::test]
    async fn a_lock_goes_with_the_process_that_held_it() {
        if let Some(file) = std::env::var_os(HOLD) {
            // The holder: hold the file, say so, wait to be killed.
            let file = PathBuf::from(file);
            let _held = RealFs.try_lock(&file).await.unwrap().unwrap();
            std::fs::write(file.with_extension("held"), "").unwrap();
            tokio::time::sleep(Duration::from_secs(60)).await;
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("s.jsonl");
        std::fs::write(&file, "{}\n").unwrap();
        let mut holder = rerun("a_lock_goes_with_the_process_that_held_it")
            .env(HOLD, &file)
            .spawn()
            .unwrap();
        let held = async {
            while !file.with_extension("held").exists() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        };
        let waited = tokio::time::timeout(Duration::from_secs(30), held).await;
        let refused_while_held = refused(&RealFs.try_lock(&file).await);
        holder.kill().unwrap();
        holder.wait().unwrap();

        waited.expect("the holder never held the file");
        assert!(refused_while_held, "another process holds it");
        let after = lock_once_free(&file).await.unwrap();
        assert!(after.is_some(), "the lock went with the process");
    }

    /// Set in a run of this test binary that tries the lock on the file it
    /// names, and leaves what became of it beside the file.
    const TRY: &str = "HAEST_TEST_TRY_LOCK";

    /// Names a directory on an NFS mount for the NFS test; unset, the test
    /// passes without running.
    const NFS_DIR: &str = "HAEST_TEST_NFS_DIR";

    /// What a `try_lock` of `file` came to in another process: `held`,
    /// `refused`, `none` (no lock) or the error.
    fn try_in_another_process(test: &str, file: &Path) -> String {
        let tried = file.with_extension("tried");
        let _ = std::fs::remove_file(&tried);
        let ran = rerun(test).env(TRY, file).status().unwrap();
        assert!(ran.success(), "the other process failed: {ran}");
        std::fs::read_to_string(tried).unwrap()
    }

    /// On NFS a lock is taken, and keeps another process out until it
    /// drops. The client takes an exclusive `flock` as a write lock on the
    /// server and refused it on the file opened only to read (EIO on macOS,
    /// EBADF on Linux), so no conversation with its data on NFS could start
    /// or resume. (Two holders in one process are one lock owner on NFSv4,
    /// so the second holder is another process.)
    #[tokio::test]
    async fn a_lock_on_nfs_keeps_another_process_out_until_it_drops() {
        let test = "a_lock_on_nfs_keeps_another_process_out_until_it_drops";
        if let Some(file) = std::env::var_os(TRY) {
            let file = PathBuf::from(file);
            let tried = match RealFs.try_lock(&file).await {
                Ok(Some(_)) => "held".to_owned(),
                Ok(None) => "none".to_owned(),
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => "refused".to_owned(),
                Err(e) => format!("error: {e}"),
            };
            std::fs::write(file.with_extension("tried"), tried).unwrap();
            return;
        }
        let Some(nfs) = std::env::var_os(NFS_DIR) else {
            return;
        };
        let dir = tempfile::tempdir_in(nfs).unwrap();
        let file = dir.path().join("s.jsonl");
        std::fs::write(&file, "{}\n").unwrap();

        let held = RealFs.try_lock(&file).await;
        assert!(matches!(held, Ok(Some(_))), "{held:?}");
        assert_eq!(try_in_another_process(test, &file), "refused");
        drop(held);
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        let mut after = try_in_another_process(test, &file);
        while after == "refused" && std::time::Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(50)).await;
            after = try_in_another_process(test, &file);
        }
        assert_eq!(after, "held", "free once the lock dropped");
    }
}
