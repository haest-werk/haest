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
//! Deletes go first, then the writes, found again: on a disk that reads two
//! spellings as one name, a delete takes the other spelling's file with it.

use std::{
    collections::{HashMap, HashSet},
    io,
    path::{Path, PathBuf},
};

use haest_core::{
    fs::Fs,
    workspace::{ApplyFailureCode, ApplyReport, ChangeKind, FailedChange},
};

use super::{Found, OverlayChanges, Scanned, links::Links, state::Original};

/// Why one change was not applied: its code and the reason for the human.
type Refusal = (ApplyFailureCode, String);

impl OverlayChanges {
    /// Every change that is not a conflict, onto the real disk: the deletes,
    /// then the writes, each in path order.
    pub(super) async fn apply_all(&self) -> ApplyReport {
        let mut report = ApplyReport::default();
        // One for the whole apply, both phases: each real path on the way is
        // asked once.
        let mut links = Links::new(self.book.disk.as_ref(), &self.book.root);
        let found = self.survey().await;
        let before: HashMap<PathBuf, Original> =
            found.iter().map(|f| (f.path.clone(), f.now)).collect();
        let (deletes, writes): (Vec<_>, Vec<_>) = found
            .into_iter()
            .filter_map(Found::change)
            .partition(|s| s.change.kind == ChangeKind::Deleted);
        let conflicts: HashSet<PathBuf> = deletes
            .iter()
            .chain(&writes)
            .filter(|s| s.change.conflict)
            .map(|s| s.path.clone())
            .collect();
        let tried = deletes.iter().any(|s| !s.change.conflict);
        for s in deletes {
            self.settle(s, &mut links, &mut report).await;
        }
        let writes = if tried {
            self.writes_after_deletes(&before, &conflicts).await
        } else {
            writes
        };
        for s in writes {
            self.settle(s, &mut links, &mut report).await;
        }
        report
    }

    /// The writes, found again once the deletes ran. On a disk that reads two
    /// spellings as one name (`readme.md` and `README.md` on macOS's
    /// default), deleting one took the other's file with it. A path there
    /// before the deletes and gone after them is this apply's doing, not the
    /// human's: it is compared from absent from now on. A path in `conflicts`
    /// (a conflict when the apply started) stays one, never compared afresh.
    async fn writes_after_deletes(
        &self,
        before: &HashMap<PathBuf, Original>,
        conflicts: &HashSet<PathBuf>,
    ) -> Vec<Scanned> {
        let mut found = self.survey().await;
        {
            let mut touched = self.book.touched();
            for f in &mut found {
                let taken = f.now == Original::Absent
                    && before
                        .get(&f.path)
                        .is_some_and(|was| *was != Original::Absent);
                if taken && !conflicts.contains(&f.path) {
                    f.original = Original::Absent;
                    if let Some(original) = touched.get_mut(&f.path) {
                        *original = Original::Absent;
                    }
                }
            }
        }
        found
            .into_iter()
            .filter_map(Found::change)
            // Each delete is in the report already.
            .filter(|s| s.change.kind != ChangeKind::Deleted)
            .map(|mut s| {
                s.change.conflict |= conflicts.contains(&s.path);
                s
            })
            .collect()
    }

    /// Put one change on the disk, or skip a conflict, into `report`.
    async fn settle(&self, s: Scanned, links: &mut Links<'_>, report: &mut ApplyReport) {
        let refused = if s.change.conflict {
            None
        } else {
            match self.put(s.change.kind, &s.path, links).await {
                Ok(()) => {
                    self.book.touched().remove(&s.path);
                    report.applied.push(s.change);
                    return;
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

    /// Write (or delete) overlay `path` on the real disk, or say why not;
    /// `links` is the apply's one lookup of links on the real disk.
    async fn put(
        &self,
        kind: ChangeKind,
        path: &Path,
        links: &mut Links<'_>,
    ) -> Result<(), Refusal> {
        let deleted = kind == ChangeKind::Deleted;
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
        self.write(deleted, path)
            .await
            .map_err(|e| (ApplyFailureCode::WriteFailed, e.to_string()))
    }

    async fn write(&self, deleted: bool, path: &Path) -> io::Result<()> {
        let disk = self.book.disk.as_ref();
        let real = self.book.real(path);
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
                _ => Ok(()),
            };
        }
        let dir = self.overlay.stat(path).await?.is_dir();
        // A directory that became a file, or a file that became a directory
        // (`rm -rf build && echo x > build`): the old one goes first.
        if disk.stat(&real).await.is_ok_and(|s| s.is_dir() != dir) {
            disk.remove_all(&real).await?;
        }
        if dir {
            return disk.create_dir_all(&real).await;
        }
        if let Some(parent) = real.parent() {
            disk.create_dir_all(parent).await?;
        }
        disk.write(&real, &self.overlay.read(path).await?).await
    }
}
