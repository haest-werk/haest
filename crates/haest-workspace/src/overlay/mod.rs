//! WHAT THE SANDBOX CHANGED, and putting it on the real disk.
//!
//! The sandbox's project overlay keeps every write in memory. [`OverlayChanges`]
//! wraps it so every MUTATION (haest's tools and the simulated shell alike --
//! both go through the overlay) records the path it touched, and what the real
//! file looked like right before: its content hash, "a directory", or "absent".
//!
//! Applying compares three things per path: the real file at first touch, the
//! real file now, and the overlay's version. If the real file moved in between
//! (the human edited it, or changed its mode), the path is a CONFLICT and is
//! skipped -- named, never overwritten; it is read once more right before its
//! write, so a move during the apply counts too. A directory the sandbox
//! removes whole (deleted, or a file in its place) waits as a conflict too
//! while a conflict waits under it: removing it would take the human's
//! version along. Everything else is written (or
//! deleted) on the real disk, deletes first, unless `apply.rs` refuses it: a
//! symlink on the real disk on its way, or a symlink the sandbox made. A
//! refused change is reported as failed with its code, and stays listed.
//! Once a change is on the disk, the sandbox reads the disk there again
//! (`through.rs`).
//!
//! The real disk is read through the system, as any program reads it
//! (`book.rs`). Where the system reads two names as one file (`README.md`
//! and `readme.md` on macOS's default disk), the sandbox may still show
//! them as two; a change that would take away what the disk holds there,
//! and so the other name's file too, is a conflict (`shown.rs`).
//!
//! Paths here are the overlay's own (`/src/a.ts`); `root` maps them to the disk.
//! A symlink is compared as a link (`state.rs`), never as what it points to.
//! Limit, stated: a directory removed whole (`rm -r dir`) is checked as a
//! directory, not file by file -- an edit the human made INSIDE it meanwhile,
//! to a file the sandbox did not change on its own, does not count as a
//! conflict, also where the sandbox makes it again and each path under it is
//! compared.

mod apply;
mod book;
mod diff;
mod links;
mod modes;
mod recorder;
mod scan;
mod shown;
mod state;
#[cfg(test)]
mod testing;
#[cfg(test)]
mod tests;
mod through;

use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
    sync::Arc,
};

use async_trait::async_trait;
use haest_core::{
    fs::{Fs, normalize},
    workspace::{ApplyReport, Change, ChangeKind, ChangeSet},
};

use crate::bridge::HaestFsOverBashkit;
use book::Book;
use recorder::Recorder;
use scan::{Found, Scanned, changes};
use state::{Original, state_of, walk};

/// What a sandbox changed in the project, and putting it on the real disk
/// (haest-core's [`ChangeSet`]).
///
/// [`SandboxWorkspace::open`](crate::SandboxWorkspace::open) builds one over
/// its project overlay; a host reaches it through
/// [`Workspace::changes`](haest_core::workspace::Workspace::changes).
///
/// Every mutation to the project -- haest's tools and the simulated shell
/// alike -- first records the path it touched
/// and what the real file looked like right then: a hash of its content, "a
/// directory", "a symlink to X", or "absent". From that:
///
/// - [`list`](ChangeSet::list): every touched path whose overlay version
///   differs from the real disk, by its path inside the project (`/src/a.ts`),
///   a directory before its files. A path whose real file changed since the
///   first touch (the human edited it) is a
///   [`conflict`](haest_core::workspace::Change::conflict); so is a
///   directory the sandbox removes whole while a conflict waits under it,
///   which removing it would take along.
/// - [`diff`](ChangeSet::diff): a unified diff of one change.
/// - [`apply`](ChangeSet::apply): writes (or deletes) every change that is not
///   a conflict, and reports each as applied, skipped or failed. An applied
///   change is no longer listed; a skipped or failed one stays, and so does
///   one an apply dropped partway (a host's timeout, Ctrl+C) had not put on
///   the disk yet.
///
/// The real disk is read through the system, as any program reads it. On
/// a disk that reads two names of a file as one (macOS's default:
/// `readme.md` and `README.md`; any APFS: `caf\u{e9}` and `cafe\u{301}`;
/// exFAT, FAT, vfat), the system decides what a name means, and the
/// sandbox, whose own files tell every name apart, may show one file on
/// the disk as two. Before a change takes away what the disk holds at its
/// path (a delete, a file written over, a directory become a file), two
/// things are asked of the disk: that each name on the way is one its
/// directory lists byte for byte, and that no other path the sandbox shows
/// (of those it changed, and the directories on their way) is the same
/// file, by device and inode number, as the path or a directory on its
/// way, or is under it. And a write lands only by names the disk lists, or
/// on a path it does not hold. Else the change would take a file the
/// sandbox shows along, or land where it does not show it: it is skipped
/// as a conflict and stays listed, and the disk keeps both names as the
/// human left them. So on such a disk a rename by case or Unicode form
/// alone (`mv README.md readme.md`, `mv lib Lib`) is held, and the human
/// renames it by hand; nothing is lost. `list` does not ask the disk so:
/// such a change is listed as no conflict until an apply skips it.
///
/// `apply` deletes first, then writes, each in path order. Each change's
/// real path is read again right before it is put on the disk, and so is
/// each change listed under a directory removed whole. One that moved since
/// the scan (the human saved or deleted it, or an earlier write of this
/// apply landed there: the sandbox wrote both `README.md` and `readme.md`)
/// is skipped as a conflict and stays listed, and so is the directory above
/// it.
///
/// `apply` never writes through a symlink: not one on the real disk on the
/// way to the path ([`OutsideProject`](haest_core::workspace::ApplyFailureCode::OutsideProject)),
/// and not one the sandbox made ([`Symlink`](haest_core::workspace::ApplyFailureCode::Symlink));
/// the human makes that link. A write the disk refuses is
/// [`WriteFailed`](haest_core::workspace::ApplyFailureCode::WriteFailed) and
/// stops nothing else.
///
/// A mode the sandbox set (`chmod`, `install -m`) is listed (`Modified`, if
/// nothing else changed; the diff names both modes, or the mode of a path
/// it adds) and applied, and so is
/// the mode a path takes along when the sandbox moves or copies it (`mv`
/// keeps it, and `cp` gives a new file its source's): a 0600 key moved or
/// copied lands 0600, and a script moved lands executable. Not so a
/// directory the disk holds that the sandbox changed something under first
/// (`echo x > certs/new`, `mkdir -p certs/sub`): from then on bashkit's
/// overlay shows it at 0755, whatever the disk's mode, and every directory
/// between it and the change too, so `mv certs keys` or `cp -r certs copy`
/// of a 0700 `certs` lands 0755. A file the
/// sandbox only edits keeps the disk's mode, and one it made lands as the
/// disk's umask makes it. Applying takes away any permission the sandbox
/// took, but grants none but execute, and that only to whom may read the
/// file: `chmod +x` gives a new file what it gives under the disk's umask,
/// and `chmod 666` or a setuid bit is not applied. A mode the human changed
/// on the disk since the sandbox set its own is a conflict.
///
/// Over [`SandboxWorkspace`](crate::SandboxWorkspace)'s project overlay, what
/// is applied reads through to the disk again: the sandbox sees what the
/// human does there afterwards (an edit, a delete, a file made again where
/// the sandbox deleted one), and its next change to such a path is checked
/// against that, never written over it. A change the sandbox makes while an
/// apply runs (a background job's) waits for the apply to end, and one under
/// way when it starts is let finish first: none is lost, nor forgotten
/// unapplied.
///
/// Limit, stated: a directory removed whole (`rm -r dir`) is checked as a
/// directory, not file by file -- an edit the human made INSIDE it meanwhile,
/// to a file the sandbox did not change on its own, does not count as a
/// conflict. Where the sandbox makes it again
/// (`rm -r dir && mkdir dir`, `rm -r dist && mv build dist`), each path under
/// it that differs, on the disk or in the sandbox, is listed, and the same
/// holds for it.
///
/// The crate docs have an example of the whole round.
pub struct OverlayChanges {
    book: Arc<Book>,
    overlay: HaestFsOverBashkit,
    /// The overlay itself, where it is bashkit's copy-on-write one: what is
    /// applied reads through to the disk again (`through.rs`).
    cow: Option<Arc<bashkit::OverlayFs>>,
}

impl std::fmt::Debug for OverlayChanges {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OverlayChanges")
            .field("root", &self.book.root)
            .finish_non_exhaustive()
    }
}

impl OverlayChanges {
    /// A change ledger over `overlay`, the copy-on-write layer over the project
    /// directory `root` on `disk`, the real file system.
    ///
    /// Nothing is recorded until mutations go through
    /// [`recorder`](Self::recorder).
    pub(crate) fn new(
        overlay: Arc<dyn bashkit::FileSystem>,
        disk: Arc<dyn Fs>,
        root: impl Into<PathBuf>,
    ) -> Self {
        Self {
            book: Arc::new(Book::new(disk, root.into())),
            overlay: HaestFsOverBashkit::new(overlay),
            cow: None,
        }
    }

    /// The same over bashkit's copy-on-write overlay of the project: once a
    /// change is applied, the sandbox reads the disk there again.
    pub(crate) fn over_cow(
        overlay: Arc<bashkit::OverlayFs>,
        disk: Arc<dyn Fs>,
        root: impl Into<PathBuf>,
    ) -> Self {
        let cow = overlay.clone();
        Self {
            cow: Some(cow),
            ..Self::new(overlay, disk, root)
        }
    }

    /// The overlay, recording what every mutation touches before it lands.
    /// Mount THIS, never the bare overlay, or edits go unrecorded.
    pub(crate) fn recorder(&self) -> Arc<dyn bashkit::FileSystem> {
        Arc::new(Recorder {
            inner: self.overlay.inner().clone(),
            book: self.book.clone(),
        })
    }

    /// Every path to compare, with what it was at first touch: the touched
    /// ones, plus everything inside a directory the sandbox CREATED (`mv d1
    /// d2`, `cp -r a b` record only `d2` / `b`, and applying that as a bare
    /// mkdir would leave it empty). Nothing under it existed on disk at first
    /// touch, so each file inside starts out absent.
    ///
    /// And a directory the disk holds that the sandbox wiped (removed whole,
    /// moved away, or moved a tree onto) and that is a directory again: the
    /// overlay hides all the disk held under it, so everything under it, on
    /// the disk and in the overlay, is compared too. Each such path starts
    /// from what the disk holds at this scan (None): the stated limit of
    /// `rm -r`, which checks no file inside.
    async fn candidates(&self) -> BTreeMap<PathBuf, Option<Original>> {
        let touched = self.book.snapshot();
        let mut all: BTreeMap<_, _> = touched.iter().map(|(p, o)| (p.clone(), Some(*o))).collect();
        // In path order, a directory comes right before those under it, which
        // its walk covers (`mkdir a a/b a/b/c` records all three).
        let mut walked: Option<&Path> = None;
        for (path, original) in &touched {
            if walked.is_some_and(|outer| path.starts_with(outer))
                || *original == Original::Dir
                || state_of(&self.overlay, path).await != Original::Dir
            {
                continue;
            }
            for inside in walk(&self.overlay, path).await {
                all.entry(inside).or_insert(Some(Original::Absent));
            }
            walked = Some(path);
        }
        for dir in self.made_again(&touched).await {
            let on_disk = self.book.tree(&dir).await;
            for inside in on_disk.into_iter().chain(walk(&self.overlay, &dir).await) {
                all.entry(inside).or_insert(None);
            }
        }
        all
    }

    /// The wiped directories that were directories on the disk at first
    /// touch and are directories in the overlay again, none under another
    /// (walking the outer one covers it).
    async fn made_again(&self, touched: &BTreeMap<PathBuf, Original>) -> Vec<PathBuf> {
        let mut out: Vec<PathBuf> = Vec::new();
        // In path order, so one comes right before those under it.
        for dir in self.book.wiped_dirs() {
            if out.last().is_some_and(|outer| dir.starts_with(outer))
                || touched.get(&dir) != Some(&Original::Dir)
                || state_of(&self.overlay, &dir).await != Original::Dir
            {
                continue;
            }
            out.push(dir);
        }
        out
    }

    /// Every path to compare as it stands, in path order (the candidates'
    /// map is ordered): a directory before its files.
    async fn survey(&self) -> Vec<Found> {
        let mut out = Vec::new();
        for (path, original) in self.candidates().await {
            let (now, mine) = tokio::join!(self.book.state(&path), state_of(&self.overlay, &path));
            let mode = self.mode_change(&path, mine).await;
            out.push(Found {
                path,
                original: original.unwrap_or(now),
                now,
                mine,
                mode,
            });
        }
        out
    }

    /// Every change, each a conflict where the human changed its path
    /// since the sandbox first read it, or where it removes a directory
    /// with such a conflict under it.
    async fn scan(&self) -> Vec<Scanned> {
        changes(self.survey().await)
    }

    /// What the sandbox shows of the paths `found` names, which an apply
    /// takes nothing of (`shown.rs`): each one it holds, and each directory
    /// on the way to any of them that it still holds (its own `Lib` around
    /// a `Lib/lib` it removed again).
    async fn shown(&self, found: &[Found]) -> Vec<PathBuf> {
        let mut shown: BTreeSet<PathBuf> = BTreeSet::new();
        let mut gone: BTreeSet<&Path> = BTreeSet::new();
        for f in found {
            let mut held = f.mine != Original::Absent;
            for at in f.path.ancestors().filter(|a| a.parent().is_some()) {
                if shown.contains(at) {
                    // And each directory above it.
                    break;
                }
                if !held && at != f.path {
                    if gone.contains(at) {
                        continue;
                    }
                    held = state_of(&self.overlay, at).await != Original::Absent;
                    if !held {
                        gone.insert(at);
                    }
                }
                if held {
                    shown.insert(at.to_path_buf());
                }
            }
        }
        shown.into_iter().collect()
    }
}

#[async_trait]
impl ChangeSet for OverlayChanges {
    /// Every path whose overlay version differs from the real disk, in path
    /// order (a directory before its files).
    async fn list(&self) -> Vec<Change> {
        self.scan().await.into_iter().map(|s| s.change).collect()
    }

    /// A unified diff of one changed text file; empty for a directory, one line
    /// for a binary file or a symlink.
    async fn diff(&self, c: &Change) -> String {
        let path = normalize(Path::new(&c.path));
        if c.kind != ChangeKind::Deleted
            && let Ok(target) = self.overlay.read_link(&path).await
        {
            return diff::link(&c.path, &target);
        }
        let before = match c.kind {
            ChangeKind::Added => Vec::new(),
            _ => diff::file_bytes(self.book.disk.as_ref(), &self.book.real(&path)).await,
        };
        let after = match c.kind {
            ChangeKind::Deleted => Vec::new(),
            _ => diff::file_bytes(&self.overlay, &path).await,
        };
        let mode = match c.kind {
            ChangeKind::Deleted => String::new(),
            _ => self.mode_line(&path, &c.path).await,
        };
        mode + &diff::unified(&c.path, &before, &after)
    }

    /// Write every non-conflicting change to the real disk, the deletes
    /// first, each skipped as a conflict if its real path moved since the
    /// scan. Returns what was applied, what was skipped (a conflict), and
    /// what failed, each with why (a symlink, a disk error). One that fails
    /// stops nothing else, and stays listed like a conflict.
    async fn apply(&self) -> ApplyReport {
        self.apply_all().await
    }
}
