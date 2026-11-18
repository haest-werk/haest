//! PUTTING ONE SANDBOX CHANGE ON THE REAL DISK, for `OverlayChanges::apply`
//! (the TS haest's `overlay-apply.ts`).
//!
//! Refused, each with its code, before anything is written:
//! - a symlink the sandbox made, or a change behind one (`symlink`): applying
//!   never makes a link, so the human makes it, from the target the reason names;
//! - a symlink on the real disk on the way, the path's own included
//!   (`outside-project`): applying writes, creates and deletes only at
//!   `root/path` itself, never through a link out of the project.
//!
//! A change the disk still refuses (permissions, a full disk) fails alone
//! (`write-failed`); the apply goes on with the rest.
//!
//! Deletes go first, then the writes: on a disk that reads two spellings as
//! one name, a rename by case (`/readme.md` deleted, `/README.md` added)
//! finds its new name free. A directory delete that frees such a name fails
//! before it starts where it would stop partway (`removable.rs`). Each
//! change's real path is read again right before it is put on the disk; one
//! that moved since the scan, or whose name or directory the disk now holds
//! under another spelling, is skipped as a conflict.

use std::{
    collections::BTreeSet,
    io,
    path::{Path, PathBuf},
};

use haest_core::{
    fs::{Fs, FsStat},
    workspace::{ApplyFailureCode, ApplyReport, ChangeKind, FailedChange},
};

use super::{OverlayChanges, Scanned, links::Links, modes, removable, state::Original};
use crate::spelling::Ways;

/// Why one change was not applied: its code and the reason for the human.
type Refusal = (ApplyFailureCode, String);

/// What became of a change `put` did not refuse.
enum Put {
    /// On the real disk now; with the names the disk held under a tree it
    /// removed (a directory deleted, or become a file).
    Applied(Vec<PathBuf>),
    /// Its real path moved since the scan, or the disk holds its name or a
    /// directory on its way under another spelling: not written, a conflict.
    Moved,
}

impl OverlayChanges {
    /// Every change that is not a conflict, onto the real disk: the deletes,
    /// then the writes, each in path order.
    pub(super) async fn apply_all(&self) -> ApplyReport {
        let mut report = ApplyReport::default();
        // One for the whole apply, deletes and writes: each real path on the
        // way is asked once.
        let mut links = Links::new(self.book.disk.as_ref(), &self.book.root);
        // The ones this scan compares under: a wipe made while the apply
        // runs, of one of these too, is not.
        let wiped = self.book.wipes();
        // Read as the sandbox spells each path, a delete takes only its own
        // name and what is under it, never a write's: one scan serves both.
        let (deletes, writes): (Vec<_>, Vec<_>) = self
            .scan()
            .await
            .into_iter()
            .partition(|s| s.change.kind == ChangeKind::Deleted);
        for s in deletes.into_iter().chain(writes) {
            self.settle(s, &mut links, &mut report).await;
        }
        self.read_through_dirs(&report.applied).await;
        // What a wiped directory hid is on the disk now, or each path of it
        // not applied is remembered (`settle`): it is forgotten, unless its
        // own change is still to apply, or the sandbox wiped it again while
        // the apply ran. A name the human makes there later is theirs, never
        // one to delete.
        let left: BTreeSet<&Path> = report
            .skipped
            .iter()
            .chain(report.failed.iter().map(|f| &f.change))
            .map(|c| Path::new(&c.path))
            .collect();
        self.book
            .forget_wiped(wiped.iter().filter(|w| !left.contains(w.dir.as_path())));
        report
    }

    /// Put one change on the disk, or skip a conflict (at the scan, or a real
    /// path that moved since), into `report`.
    async fn settle(&self, mut s: Scanned, links: &mut Links<'_>, report: &mut ApplyReport) {
        let refused = if s.change.conflict {
            None
        } else {
            match self.put(&s, links).await {
                Ok(Put::Applied(took)) => {
                    self.read_through(&s, &took).await;
                    self.book.touched().remove(&s.path);
                    self.book.modes.forget(&s.path);
                    report.applied.push(s.change);
                    return;
                }
                Ok(Put::Moved) => {
                    s.change.conflict = true;
                    None
                }
                Err(refusal) => Some(refusal),
            }
        };
        // A file found inside a created directory: remember it, so it is
        // still listed once its directory is applied.
        self.book.touched().entry(s.path).or_insert(s.original);
        match refused {
            Some((code, reason)) => report
                .failed
                .push(FailedChange::new(s.change, code, reason)),
            None => report.skipped.push(s.change),
        }
    }

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

    /// Write (or delete) `s` on the real disk, find its real path moved, or
    /// say why not; `links` is the apply's one lookup of links on the real
    /// disk.
    async fn put(&self, s: &Scanned, links: &mut Links<'_>) -> Result<Put, Refusal> {
        let path = s.path.as_path();
        let deleted = s.change.kind == ChangeKind::Deleted;
        let link = self.sandbox_link(path, deleted).await;
        if let Some((at, target)) = &link
            && at == path
        {
            return Err((
                ApplyFailureCode::Symlink,
                format!(
                    "a symlink to {}: applying does not make links, so make it yourself",
                    target.display()
                ),
            ));
        }
        // Checked at the apply, not at the scan: a link may have appeared on
        // disk since.
        if links.through_a_link(path).await {
            return Err((
                ApplyFailureCode::OutsideProject,
                "a symlink on the real disk is on its way, and applying never writes through one"
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
        // Read again right before the write: the human may have changed it
        // since the scan, and on a disk that reads two spellings as one name,
        // the disk may hold it, or a directory on its way, under another
        // spelling now (an earlier write of this apply, a delete that failed
        // or was skipped), where the write or delete would land. A path that
        // already holds the sandbox's version (a file its directory's delete
        // took along, a change of mode alone) loses nothing, and is put
        // without writing it again. Asked afresh: a directory this apply
        // made changes the answer.
        let again = self.book.on_disk(path, &mut Ways::default()).await;
        let moved = again.state != s.now && again.state != s.mine;
        if moved || again.held.is_some() {
            return Ok(Put::Moved);
        }
        self.write(s, again.state == s.mine)
            .await
            .map(Put::Applied)
            .map_err(|e| (ApplyFailureCode::WriteFailed, e.to_string()))
    }

    /// Put `s` on the disk; `holds` when the disk holds the sandbox's
    /// version already, and only its mode may be left to put. Returns the
    /// names the disk held under a tree it removed.
    async fn write(&self, s: &Scanned, holds: bool) -> io::Result<Vec<PathBuf>> {
        let path = s.path.as_path();
        let deleted = s.change.kind == ChangeKind::Deleted;
        let disk = self.book.disk.as_ref();
        let real = self.book.real(path);
        let found = disk.stat(&real).await.ok();
        let was_dir = found.as_ref().is_some_and(FsStat::is_dir);
        // A directory whose name this apply's writes land on under another
        // spelling (a rename by case) goes whole or not at all: what a
        // delete took before it stopped would otherwise be only in the
        // sandbox, its writes waiting on the name.
        if deleted
            && s.frees
            && was_dir
            && let Some(blocker) = removable::blocker(&real).await
        {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!(
                    "{} may not be emptied or changed, so the delete would stop partway; \
                     nothing was deleted",
                    blocker.display()
                ),
            ));
        }
        // The names under a directory this change removes, which the
        // overlay hides until the apply has read through (`through.rs`).
        let dir = !deleted && self.overlay.stat(path).await?.is_dir();
        let took = if was_dir && !dir && self.cow.is_some() {
            self.book.tree(path).await
        } else {
            Vec::new()
        };
        if deleted {
            return match disk.remove_all(&real).await {
                // Gone already, or under a directory that became a file.
                Err(e)
                    if !matches!(
                        e.kind(),
                        io::ErrorKind::NotFound | io::ErrorKind::NotADirectory
                    ) =>
                {
                    Err(e)
                }
                _ => Ok(took),
            };
        }
        // A directory that became a file, or a file that became a directory
        // (`rm -rf build && echo x > build`): the old one goes first.
        if found.is_some_and(|st| st.is_dir() != dir) {
            disk.remove_all(&real).await?;
        }
        if dir {
            disk.create_dir_all(&real).await?;
        } else if !holds {
            if let Some(parent) = real.parent() {
                disk.create_dir_all(parent).await?;
            }
            disk.write(&real, &self.overlay.read(path).await?).await?;
        }
        self.carry_mode(s, &real).await?;
        Ok(took)
    }

    /// Put the mode the sandbox chose for `s` on the disk at `real`: the one
    /// it set, or the execute bits of a file it added (`modes.rs`).
    async fn carry_mode(&self, s: &Scanned, real: &Path) -> io::Result<()> {
        let Some(disk) = modes::on_disk(real).await else {
            return Ok(());
        };
        let want = if let Some(set) = self.book.modes.get(&s.path) {
            modes::carried(disk, set.mode)
        } else if s.change.kind == ChangeKind::Added && matches!(s.mine, Original::File(_)) {
            match self.overlay.inner().stat(&s.path).await {
                Ok(mine) => modes::added(disk, mine.mode),
                Err(_) => disk,
            }
        } else {
            disk
        };
        if want == disk {
            return Ok(());
        }
        self.book.disk.chmod(real, want).await
    }
}

#[cfg(test)]
mod tests;
