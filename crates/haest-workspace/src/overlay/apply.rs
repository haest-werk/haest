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
//! A file goes on the disk whole or not at all, through the disk's whole
//! write ([`Fs::write_whole`]): on the real disk, a new file beside it that
//! takes its permission bits and is renamed over it (`RealFs`'s doc says
//! where it writes in place instead, and what a file replaced so does not
//! keep). A change the disk refuses (permissions, a full disk, an NFS
//! server out of room, which says so only as the new file closes) fails
//! alone (`write-failed`) with the human's file as it was, still listed
//! (not a conflict) for an apply once the disk takes it; the apply goes on
//! with the rest. A file the human made read-only is refused, never
//! replaced. A file written in place that the disk stops partway (a full
//! disk, an NFS server out of room) is still cut short: the change fails,
//! and later lists show it as a conflict. A kill partway can leave the new
//! file beside it, never a file cut short: its name, `.haest-*`, matches
//! none of the file's own ignore patterns.
//!
//! The writes go first and the deletes last, so nothing is taken away
//! before the write that carries it elsewhere has landed: first each write
//! that takes nothing away (a new path, a change of mode alone), then each
//! that does (a file written over, a directory become a file or the other
//! way round) with what a file become a directory holds, then each delete.
//! Each group goes in path order, but the second: each of those goes after
//! another of them whose write carries its content ([`in_order`]). One that
//! takes away (a delete too) what a write of this apply that did not land
//! carries elsewhere (a move, a copy, or the same content: `carried.rs`) is
//! skipped as a conflict, and stays listed. Each directory's mode goes on
//! last, deepest first, so one made read-only still gets what was written
//! into it. Each change's real path is read
//! again right before it is put on the disk; one that moved since the scan
//! is skipped as a conflict. A directory removed whole is read again with
//! each change listed under it: one of them that moved keeps the directory
//! as a conflict too, never removed with it. And one that would take along
//! a file the sandbox shows under another name the disk reads as the same,
//! or land under a name the disk lists another way (`shown.rs`), is a
//! conflict too.

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
    OverlayChanges, Scanned,
    carried::{Carried, Waiting, in_order},
    links::Links,
    modes,
    scan::{changes, under},
    shown::Shown,
    state::{Original, state_of, walk},
};

/// Why one change was not applied: its code and the reason for the human.
type Refusal = (ApplyFailureCode, String);

/// One apply, as it goes through the changes.
struct Run<'a> {
    /// The apply's one lookup of links on the real disk: each real path on
    /// the way is asked once, for the deletes and the writes.
    links: Links<'a>,
    /// What the sandbox changed and still shows, which no change takes
    /// away (`shown.rs`).
    shown: Shown,
    /// What the scan found at each path it listed: on the disk, then in the
    /// overlay.
    listed: BTreeMap<PathBuf, (Original, Original)>,
    /// Where the sandbox moved and copied what the disk holds.
    carried: Carried,
    /// The writes so far that did not land.
    waiting: Waiting,
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
    /// Its real path moved since the scan, putting it would take along
    /// another file the sandbox shows, or it takes away what a write that
    /// did not land carries: not written, a conflict.
    Moved,
}

impl OverlayChanges {
    /// Every change that is not a conflict, onto the real disk: the writes
    /// that take nothing away, those that do, then the deletes.
    pub(super) async fn apply_all(&self) -> ApplyReport {
        // The sandbox's writes wait for the apply, and the apply for those
        // under way: between the scan and the end, the overlay holds still.
        let _alone = self.book.gate.write().await;
        // The ones this scan compares under: a wipe made while the apply
        // runs, of one of these too, is not.
        let wiped = self.book.wipes();
        let found = self.survey().await;
        let shown = self.shown(&found).await;
        let scanned = changes(found);
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
            shown: Shown::new(&self.book.root, shown),
            listed: scanned
                .iter()
                .map(|s| (s.path.clone(), (s.now, s.mine)))
                .collect(),
            carried: Carried::new(self.book.carried()),
            waiting: Waiting::default(),
            report: ApplyReport::default(),
            dirs: Vec::new(),
        };
        let (deletes, writes): (Vec<_>, Vec<_>) = scanned
            .into_iter()
            .partition(|s| s.change.kind == ChangeKind::Deleted);
        let (takes, plain): (Vec<_>, Vec<_>) = writes.into_iter().partition(Scanned::takes);
        // What a file become a directory holds is written once it is one.
        let made: BTreeSet<PathBuf> = takes
            .iter()
            .filter(|s| s.mine == Original::Dir)
            .map(|s| s.path.clone())
            .collect();
        let (inside, plain): (Vec<_>, Vec<_>) = plain
            .into_iter()
            .partition(|s| s.path.ancestors().skip(1).any(|a| made.contains(a)));
        for s in plain {
            self.settle(s, &mut run).await;
        }
        // Each phase asks the disk as the writes before it left it: what
        // this apply wrote is what the sandbox shows there too.
        run.shown.refresh();
        for s in in_order(takes, &run.carried).into_iter().chain(inside) {
            self.settle(s, &mut run).await;
        }
        run.shown.refresh();
        for s in deletes {
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
        self.book.forget_carried();
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
        // A write that did not land: what it carries stays where it is.
        if s.change.kind != ChangeKind::Deleted {
            let len = match s.mine {
                Original::File(_) => self.overlay.stat(&s.path).await.ok().map(|st| st.size),
                _ => None,
            };
            run.waiting.add(&s, len);
        }
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
        // since the scan, or an earlier write of this apply landed there
        // under another name the disk reads as this one (the sandbox wrote
        // both `README.md` and `readme.md`). A path that already holds the
        // sandbox's version (a file its directory's delete took along, a
        // change of mode alone) loses nothing, and is put without writing it
        // again.
        let again = self.book.state(path).await;
        if again != s.now && again != s.mine {
            return Ok(Put::Moved);
        }
        // A directory removed whole takes along what is under it: a change
        // listed there that moved since the scan would go too, while it is
        // reported skipped.
        if again == Original::Dir && s.removes_tree() && self.moved_under(path, &run.listed).await {
            return Ok(Put::Moved);
        }
        // Where the disk reads two names as one file, the sandbox may show
        // it as two: a delete or a write of one takes the other along, and
        // a write into the sandbox's own `Lib` lands in the disk's `lib`
        // (`shown.rs`).
        let holds = again == s.mine;
        if (deleted || !holds) && run.shown.would_take(path).await {
            return Ok(Put::Moved);
        }
        // It takes away what the disk holds: not while a write that did not
        // land carries that elsewhere (`carried.rs`).
        if !holds && again != Original::Absent && self.carries(path, again, run).await {
            return Ok(Put::Moved);
        }
        let there = again != Original::Absent;
        if !deleted && run.shown.lands_elsewhere(path, there).await {
            return Ok(Put::Moved);
        }
        let took = self
            .write(s, holds)
            .await
            .map_err(|e| (ApplyFailureCode::WriteFailed, e.to_string()))?;
        run.shown.put(path, deleted);
        Ok(Put::Applied(took))
    }

    /// Whether a write of this apply that did not land carries what the
    /// disk holds at `path` (`again`: a file, or a directory with all under
    /// it) elsewhere: a move or a copy the sandbox made from there, or a
    /// file with the same content.
    async fn carries(&self, path: &Path, again: Original, run: &Run<'_>) -> bool {
        if run.waiting.is_empty() {
            return false;
        }
        if run.waiting.at_any(&run.carried.reached(path)) {
            return true;
        }
        let disk = self.book.disk.as_ref();
        let real = self.book.real(path);
        let files = match again {
            Original::File(_) => vec![real],
            Original::Dir => walk(disk, &real).await,
            _ => return false,
        };
        for file in files {
            // Each file whose length a waiting write has, read to tell.
            let Ok(st) = disk.stat(&file).await else {
                continue;
            };
            if st.is_dir() || !run.waiting.may_hold(st.size) {
                continue;
            }
            if let Original::File(hash) = state_of(disk, &file).await
                && run.waiting.holds(st.size, hash)
            {
                return true;
            }
        }
        false
    }

    /// Whether a change `listed` under `dir` moved on the disk since the
    /// scan: it holds neither what the scan found there nor the sandbox's
    /// version (the human saved or made it meanwhile).
    async fn moved_under(
        &self,
        dir: &Path,
        listed: &BTreeMap<PathBuf, (Original, Original)>,
    ) -> bool {
        for (path, &(now, mine)) in under(listed, dir) {
            let again = self.book.state(path).await;
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
            let bytes = self.overlay.read(path).await?;
            disk.write_whole(&real, &bytes).await?;
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
