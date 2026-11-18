//! Holding a session for one writer: [`JsonlSessionFile::try_lock`].

use std::io;

use super::{JsonlSessionFile, dir_of, private_dir};

impl JsonlSessionFile {
    /// Hold the session for this writer alone until this value is dropped:
    /// meanwhile another writer's `try_lock` of the session, in this process
    /// or another, fails with `WouldBlock`. On a file system with no locks
    /// it holds nothing, and succeeds. Once it holds the session, it returns
    /// at once.
    ///
    /// What it holds is [`Fs::try_lock`](crate::fs::Fs::try_lock) on
    /// `<id>.lock` beside the session file, which nothing reads or writes:
    /// where locks are mandatory (SMB), a lock on the session file itself
    /// would refuse the writer its own reads and writes. The lock file is
    /// made when missing and stays when the session ends: one removed while
    /// another writer opens it would leave the session two lock files. For
    /// a new session (no file yet), the sessions dir is first made private
    /// (0700, and made when missing), as [`create`](Self::create) leaves
    /// it, so the lock file can be made there; a saved session's dir is left
    /// as it is. Where the dir cannot be made private, the lock is tried all
    /// the same: `create` needs the dir too, and fails with the dir's own
    /// error.
    ///
    /// A saved session whose lock file cannot be opened or made, for a
    /// reason that says nothing of whether the file system locks, is held
    /// through the session file itself: the lock file is another user's
    /// (`PermissionDenied`), or the dir takes no new file (`PermissionDenied`
    /// too, `ReadOnlyFilesystem`, `QuotaExceeded`, `StorageFull`). A new
    /// session has no file to hold, and gets the error. Two writers keep
    /// each other out only while they hold the same file, so one held
    /// through the session file and one that holds `<id>.lock` do not. That
    /// takes runs as two users (the one whose lock file it is, and one that
    /// cannot open it), or a dir or disk that took a new file again between
    /// the two runs. Where locks are mandatory, a session held through its
    /// own file refuses its writer its own reads and writes, as above.
    ///
    /// Take it before the first record of a session another writer may open:
    /// before [`create`](Self::create) for a new one, so a session in use
    /// leaves no file, and before [`load`](Self::load) for a saved one. The
    /// lock goes as this value is dropped, before its queued records land:
    /// [`flush`](super::SessionSink::flush) first.
    ///
    /// # Errors
    ///
    /// `WouldBlock` when another writer holds the session, and the file
    /// system's other errors from taking the lock (on the session file,
    /// where it is held through that).
    pub async fn try_lock(&self) -> io::Result<()> {
        let mut held = self.held.lock().await;
        if held.is_none() {
            let saved = self.fs.exists(&self.path).await;
            if !saved {
                // `create` makes the dir private too, and says why it cannot.
                let _ = private_dir(&*self.fs, dir_of(&self.path)).await;
            }
            *held = match self.fs.try_lock(&self.path.with_extension("lock")).await {
                Err(e) if saved && cannot_open(&e) => self.fs.try_lock(&self.path).await?,
                lock => lock?,
            };
        }
        Ok(())
    }
}

/// An error opening or making a lock file that says nothing of whether the
/// file system locks: the file is another user's, or the dir takes no new
/// file (its mode, a read-only mount, a quota, a full disk).
fn cannot_open(e: &io::Error) -> bool {
    matches!(
        e.kind(),
        io::ErrorKind::PermissionDenied
            | io::ErrorKind::ReadOnlyFilesystem
            | io::ErrorKind::QuotaExceeded
            | io::ErrorKind::StorageFull
    )
}
