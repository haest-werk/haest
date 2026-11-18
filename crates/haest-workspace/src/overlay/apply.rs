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
//! A file goes on the disk whole or not at all (`real/replace.rs`): the
//! sandbox's version is written to a new file in a directory of its own
//! beside it that only its owner may enter from the moment it is made (a
//! new file that replaces one is its owner's alone too, before the bytes
//! go in); the new file then takes the file's permission bits and is
//! renamed over it. That directory's mode is never set after it is made,
//! so where the file's directory passes its group on (setgid), the new
//! file gets that group, as one made beside the file would, also for a
//! run not in that group. A change the disk still
//! refuses (permissions, a full disk, an NFS server out of room, which
//! says so only as the new file closes) fails alone (`write-failed`) with
//! the human's file as it was, still listed (not a conflict) for an apply
//! once the disk takes it; the apply goes on with the rest. A file the human
//! made read-only is refused, never replaced. Written in place, as before:
//! a file a new one would not stand in for (one with another hard link,
//! one whose owner or group a new file would not have), and one where the
//! disk will not make the new file, make it its owner's alone, or move it
//! there (a directory that takes no new name, a share mounted for one
//! fixed owner the run is not, a path the longer name takes past the
//! system's cap, a file that is a mount point of its own). A new file the
//! disk has no room for fails the change: in place, the bytes would cut
//! the file short. A file written in place that the disk stops partway (a
//! full disk, an NFS server out of room) is still cut short: the change
//! fails, and later lists show it as a conflict.
//! The trade-offs of a file replaced so:
//! - it does not keep its extended attributes or ACLs;
//! - a process that holds it open to write (a log a server appends to, a
//!   database) goes on writing to the old file: nothing it writes after
//!   the apply shows at the path;
//! - the disk needs room for a second copy of it while the new one fills;
//!   a file written in place needs none;
//! - a kill partway can leave the new file's directory,
//!   `.<name>.haest-<pid>-<n>`, beside it, with the new file in it, which
//!   only its owner may reach; never a file cut short. An apply dropped
//!   just as the system starts making that directory can leave it empty.
//!   The name matches none of the file's own ignore patterns; `.*.haest-*`
//!   matches every one.
//!
//! Deletes go first, then the writes: on a disk that reads two spellings as
//! one name, a rename by case (`/readme.md` deleted, `/README.md` added)
//! finds its new name free. Each directory's mode goes on last, deepest
//! first, so one made read-only still gets what was written into it. A directory delete that frees such a name fails
//! before it starts where it would stop partway (`removable.rs`). Each
//! change's real path is read again right before it is put on the disk; one
//! that moved since the scan, or whose name or directory the disk now holds
//! under another spelling, is skipped as a conflict. A directory removed
//! whole is read again with each change listed under it: one of them that
//! moved keeps the directory as a conflict too, never removed with it.

use std::{
    collections::{BTreeMap, BTreeSet},
    io,
    path::{Path, PathBuf},
};

use haest_core::{
    fs::{Fs, FsStat},
    workspace::{ApplyFailureCode, ApplyReport, ChangeKind, FailedChange},
};

use super::{
    OverlayChanges, Scanned, links::Links, modes, removable, scan::under, state::Original,
};
use crate::{real::put_file, spelling::Ways};

/// Why one change was not applied: its code and the reason for the human.
type Refusal = (ApplyFailureCode, String);

/// One apply, as it goes through the changes.
struct Run<'a> {
    /// The apply's one lookup of links on the real disk: each real path on
    /// the way is asked once, for the deletes and the writes.
    links: Links<'a>,
    /// What the scan found at each path it listed: on the disk, then in the
    /// overlay.
    listed: BTreeMap<PathBuf, (Original, Original)>,
    report: ApplyReport,
    /// Each directory put on the disk, by its place in `report.applied`:
    /// its mode goes on once everything under it is written.
    dirs: Vec<(usize, PathBuf)>,
}

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
        // The sandbox's writes wait for the apply, and the apply for those
        // under way: between the scan and the end, the overlay holds still.
        let _alone = self.book.gate.write().await;
        // The ones this scan compares under: a wipe made while the apply
        // runs, of one of these too, is not.
        let wiped = self.book.wipes();
        // Read as the sandbox spells each path, a delete takes only its own
        // name and what is under it, never a write's: one scan serves both.
        let scanned = self.scan().await;
        // Each change stays in the ledger until it is on the disk. What is
        // under a directory the sandbox made is found by walking it, so once
        // that directory is on the disk, a path under it not written yet
        // (skipped, failed, or not reached by an apply dropped partway, as
        // Ctrl+C at the haest command's `/apply` does) would be listed
        // nowhere.
        {
            let mut touched = self.book.touched();
            for s in &scanned {
                touched.entry(s.path.clone()).or_insert(s.original);
            }
        }
        let mut run = Run {
            links: Links::new(self.book.disk.as_ref(), &self.book.root),
            listed: scanned
                .iter()
                .map(|s| (s.path.clone(), (s.now, s.mine)))
                .collect(),
            report: ApplyReport::default(),
            dirs: Vec::new(),
        };
        let (deletes, writes): (Vec<_>, Vec<_>) = scanned
            .into_iter()
            .partition(|s| s.change.kind == ChangeKind::Deleted);
        for s in deletes.into_iter().chain(writes) {
            self.settle(s, &mut run).await;
        }
        let mut report = run.report;
        // Deepest first, and after every write: `chmod 555 out` after a
        // write into it must not stop that write. Taken from the end, so
        // each place in `applied` still holds its directory.
        for (at, path) in run.dirs.into_iter().rev() {
            self.dir_mode(at, &path, &mut report).await;
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
    /// path that moved since), into the run's report.
    async fn settle(&self, mut s: Scanned, run: &mut Run<'_>) {
        let refused = if s.change.conflict {
            None
        } else {
            match self.put(&s, run).await {
                Ok(Put::Applied(took)) => {
                    self.read_through(&s, &took).await;
                    if s.mine == Original::Dir {
                        // Its mode goes on last (`dir_mode`): till then it
                        // stays in the ledger, as the directory the disk
                        // holds now, so an apply dropped first leaves the
                        // mode listed.
                        self.book.touched().insert(s.path.clone(), Original::Dir);
                        let at = run.report.applied.len();
                        run.dirs.push((at, s.path));
                    } else {
                        self.book.touched().remove(&s.path);
                        self.book.modes.forget(&s.path);
                    }
                    run.report.applied.push(s.change);
                    return;
                }
                Ok(Put::Moved) => {
                    s.change.conflict = true;
                    None
                }
                Err(refusal) => Some(refusal),
            }
        };
        // Still in the ledger (`apply_all`), so still listed.
        match refused {
            Some((code, reason)) => run
                .report
                .failed
                .push(FailedChange::new(s.change, code, reason)),
            None => run.report.skipped.push(s.change),
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
    /// say why not.
    async fn put(&self, s: &Scanned, run: &mut Run<'_>) -> Result<Put, Refusal> {
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
        if run.links.through_a_link(path).await {
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
        // A directory removed whole takes along what is under it: a change
        // listed there that moved since the scan would go too, while it is
        // reported skipped.
        if again.state == Original::Dir
            && s.removes_tree()
            && self.moved_under(path, &run.listed).await
        {
            return Ok(Put::Moved);
        }
        self.write(s, again.state == s.mine)
            .await
            .map(Put::Applied)
            .map_err(|e| (ApplyFailureCode::WriteFailed, e.to_string()))
    }

    /// Whether a change `listed` under `dir` moved on the disk since the
    /// scan: it holds neither what the scan found there nor the sandbox's
    /// version (the human saved or made it meanwhile).
    async fn moved_under(
        &self,
        dir: &Path,
        listed: &BTreeMap<PathBuf, (Original, Original)>,
    ) -> bool {
        let mut ways = Ways::default();
        for (path, &(now, mine)) in under(listed, dir) {
            let again = self.book.on_disk(path, &mut ways).await.state;
            if again != now && again != mine {
                return true;
            }
        }
        false
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
            // Its mode goes on once all under it is written (`dir_mode`).
            disk.create_dir_all(&real).await?;
            return Ok(took);
        }
        if !holds {
            if let Some(parent) = real.parent() {
                disk.create_dir_all(parent).await?;
            }
            put_file(disk, &real, &self.overlay.read(path).await?).await?;
        }
        self.carry_mode(path, s.change.kind, s.mine, &real).await?;
        Ok(took)
    }

    /// Put the mode of the directory at `path`, applied as `report.applied`'s
    /// `at`, on the disk. One the disk refuses moves to `failed`, and stays
    /// listed for its mode.
    async fn dir_mode(&self, at: usize, path: &Path, report: &mut ApplyReport) {
        let kind = report.applied[at].kind;
        let done = self
            .carry_mode(path, kind, Original::Dir, &self.book.real(path))
            .await;
        match done {
            Ok(()) => {
                self.book.touched().remove(path);
                self.book.modes.forget(path);
            }
            Err(e) => {
                // Still in the ledger as the directory the disk holds now
                // (`settle`): only its mode is left.
                let change = report.applied.remove(at);
                let code = ApplyFailureCode::WriteFailed;
                report
                    .failed
                    .push(FailedChange::new(change, code, e.to_string()));
            }
        }
    }

    /// Put the mode the sandbox chose for `path`, a `kind` change to `mine`,
    /// on the disk at `real`: the one it set, or the execute bits of a file
    /// it added (`modes.rs`).
    async fn carry_mode(
        &self,
        path: &Path,
        kind: ChangeKind,
        mine: Original,
        real: &Path,
    ) -> io::Result<()> {
        let Some(disk) = modes::on_disk(real).await else {
            return Ok(());
        };
        let want = if let Some(set) = self.book.modes.get(path) {
            modes::carried(disk, set.mode)
        } else if kind == ChangeKind::Added && matches!(mine, Original::File(_)) {
            match self.overlay.inner().stat(path).await {
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
