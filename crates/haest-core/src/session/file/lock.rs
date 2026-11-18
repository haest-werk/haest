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
    /// Take it before the first record of a session another writer may open:
    /// before [`create`](Self::create) for a new one, so a session in use
    /// leaves no file, and before [`load`](Self::load) for a saved one. The
    /// lock goes as this value is dropped, before its queued records land:
    /// [`flush`](super::SessionSink::flush) first.
    ///
    /// # Errors
    ///
    /// `WouldBlock` when another writer holds the session, and the file
    /// system's other errors from taking the lock.
    pub async fn try_lock(&self) -> io::Result<()> {
        let mut held = self.held.lock().await;
        if held.is_none() {
            if !self.fs.exists(&self.path).await {
                // `create` makes the dir private too, and says why it cannot.
                let _ = private_dir(&*self.fs, dir_of(&self.path)).await;
            }
            *held = self.fs.try_lock(&self.path.with_extension("lock")).await?;
        }
        Ok(())
    }
}
