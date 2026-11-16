//! PUTTING ONE SANDBOX CHANGE ON THE REAL DISK, for `OverlayChanges::apply`
//! (the TS haest's `overlay-apply.ts`).
//!
//! Refused, each with its code, before anything is written:
//! - a symlink the sandbox made, or a change behind one (`symlink`): /apply
//!   never makes a link, so the human makes it, from the target the reason names;
//! - a symlink on the real disk on the way, the path's own included
//!   (`outside-project`): /apply writes, creates and deletes only at
//!   `root/path` itself, never through a link out of the project.
//!
//! A change the disk still refuses (permissions, a full disk) fails alone
//! (`write-failed`); the caller goes on with the rest.

use std::{
    io,
    path::{Path, PathBuf},
};

use haest_core::{
    fs::Fs,
    workspace::{ApplyFailureCode, ChangeKind},
};

use super::{OverlayChanges, links::Links};

/// Why one change was not applied: its code and the reason for the human.
pub(crate) type Refusal = (ApplyFailureCode, String);

impl OverlayChanges {
    /// The nearest symlink in the sandbox at `path` or on its way there, with
    /// its target. A deleted path is looked up from its parent: what is gone
    /// is not a link.
    async fn sandbox_link(&self, path: &Path, deleted: bool) -> Option<(PathBuf, PathBuf)> {
        let mut at = if deleted { path.parent() } else { Some(path) };
        while let Some(p) = at.filter(|p| p.parent().is_some()) {
            if let Ok(target) = self.overlay.read_link(p).await {
                return Some((p.to_path_buf(), target));
            }
            at = p.parent();
        }
        None
    }

    /// Write (or delete) overlay `path` on the real disk, or say why not.
    pub(crate) async fn put(&self, kind: ChangeKind, path: &Path) -> Result<(), Refusal> {
        let deleted = kind == ChangeKind::Deleted;
        let link = self.sandbox_link(path, deleted).await;
        if let Some((at, target)) = &link
            && at == path
        {
            return Err((
                ApplyFailureCode::Symlink,
                format!(
                    "a symlink to {}: /apply does not make links, so make it yourself",
                    target.display()
                ),
            ));
        }
        // Checked now, not at the scan: a link may have appeared on disk since.
        let disk = self.book.disk.as_ref();
        if Links::new(disk, &self.book.root).through_a_link(path).await {
            return Err((
                ApplyFailureCode::OutsideProject,
                "a symlink on the real disk is on its way, and /apply never writes through one"
                    .into(),
            ));
        }
        if let Some((at, _)) = link {
            return Err((
                ApplyFailureCode::Symlink,
                format!(
                    "it is inside {}, a symlink the sandbox made",
                    at.to_string_lossy()
                ),
            ));
        }
        self.write(deleted, path)
            .await
            .map_err(|e| (ApplyFailureCode::WriteFailed, e.to_string()))
    }

    async fn write(&self, deleted: bool, path: &Path) -> io::Result<()> {
        let disk = self.book.disk.as_ref();
        let real = self.book.real(path);
        if deleted {
            return match disk.remove(&real, true).await {
                // Gone already, or under a directory that became a file.
                Err(e)
                    if !matches!(
                        e.kind(),
                        io::ErrorKind::NotFound | io::ErrorKind::NotADirectory
                    ) =>
                {
                    Err(e)
                }
                _ => Ok(()),
            };
        }
        let dir = self.overlay.stat(path).await?.is_dir();
        // A directory that became a file, or a file that became a directory
        // (`rm -rf build && echo x > build`): the old one goes first.
        if disk.stat(&real).await.is_ok_and(|s| s.is_dir() != dir) {
            disk.remove(&real, true).await?;
        }
        if dir {
            return disk.mkdir(&real, true).await;
        }
        if let Some(parent) = real.parent() {
            disk.mkdir(parent, true).await?;
        }
        disk.write(&real, &self.overlay.read(path).await?).await
    }
}
