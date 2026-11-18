//! WHAT THE SANDBOX CHANGED, and putting it on the real disk.
//!
//! The sandbox's project overlay keeps every write in memory. [`OverlayChanges`]
//! wraps it so every MUTATION (haest's tools and the simulated shell alike --
//! both go through the overlay) records the path it touched, and what the real
//! file looked like right before: its content hash, "a directory", or "absent".
//!
//! Applying compares three things per path: the real file at first touch, the
//! real file now, and the overlay's version. If the real file moved in between
//! (the human edited it), the path is a CONFLICT and is skipped -- named, never
//! overwritten; it is read once more right before its write, so a move during
//! the apply counts too. Everything else is written (or deleted) on the real disk,
//! deletes first, unless `apply.rs` refuses it: a symlink on the real disk on
//! its way, or a symlink the sandbox made. A refused change is reported as
//! failed with its code, and stays listed.
//!
//! The real disk is read as the sandbox spells each path (`book.rs`): on a
//! disk that reads `Lib` as `lib`, `/Lib` is absent while `lib` is there.
//!
//! Paths here are the overlay's own (`/src/a.ts`); `root` maps them to the disk.
//! A symlink is compared as a link (`state.rs`), never as what it points to.
//! Limit, stated: a directory removed whole (`rm -r dir`) is checked as a
//! directory, not file by file -- an edit the human made INSIDE it meanwhile
//! does not count as a conflict, also where the sandbox makes it again and
//! each path under it is compared.

mod apply;
mod book;
mod diff;
mod links;
mod recorder;
mod state;
#[cfg(test)]
mod testing;
#[cfg(test)]
mod tests;

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::Arc,
};

use async_trait::async_trait;
use haest_core::{
    fs::{Fs, normalize},
    workspace::{ApplyReport, Change, ChangeKind, ChangeSet},
};

use crate::bridge::HaestFsOverBashkit;
use book::{Book, Held};
use recorder::Recorder;
use state::{Original, state_of, walk};

/// One path to compare, as it stands: at first touch, on the real disk now
/// (both as the sandbox spells the path), and in the overlay.
struct Found {
    path: PathBuf,
    original: Original,
    now: Original,
    /// Where the disk holds the path's name, or a directory on its way,
    /// under another spelling.
    held: Option<Held>,
    mine: Original,
}

impl Found {
    /// A delete that is not a conflict: applying frees its name.
    fn frees(&self) -> bool {
        self.mine == Original::Absent && self.now != Original::Absent && self.original == self.now
    }

    /// The change the overlay makes here; None when it holds what the disk
    /// does. `freed` are the names this change set's deletes free.
    fn change(self, freed: &[PathBuf]) -> Option<Scanned> {
        if self.now == self.mine {
            return None;
        }
        let kind = match (self.mine, self.now) {
            (Original::Absent, _) => ChangeKind::Deleted,
            (_, Original::Absent) => ChangeKind::Added,
            _ => ChangeKind::Modified,
        };
        // Applied, it would land on what the disk holds under another
        // spelling, itself or a directory on its way, unless a delete frees
        // that name first (a rename by case). Even where that holds the
        // sandbox's version already: once applied, the change leaves the
        // list while the sandbox still shows the two names apart, and a
        // delete of the other name would take it along.
        let lands_on_another = self.held.is_some_and(|h| {
            !h.at
                .is_some_and(|at| freed.iter().any(|d| at.starts_with(d)))
        });
        Some(Scanned {
            change: Change::new(self.path.to_string_lossy(), kind)
                .with_conflict(self.original != self.now || lands_on_another),
            path: self.path,
            original: self.original,
            now: self.now,
            mine: self.mine,
        })
    }
}

/// The changes among `found`, in its order.
fn changes(found: Vec<Found>) -> Vec<Scanned> {
    let freed: Vec<PathBuf> = found
        .iter()
        .filter(|f| f.frees())
        .map(|f| f.path.clone())
        .collect();
    found.into_iter().filter_map(|f| f.change(&freed)).collect()
}

/// One change, and the states it was found from.
struct Scanned {
    change: Change,
    path: PathBuf,
    original: Original,
    /// The real path when scanned: what applying expects to find there.
    now: Original,
    /// The overlay's version when scanned.
    mine: Original,
}

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
///   first touch (the human edited it), or that would land on a name the
///   disk holds under another spelling (below), is a
///   [`conflict`](haest_core::workspace::Change::conflict).
/// - [`diff`](ChangeSet::diff): a unified diff of one change.
/// - [`apply`](ChangeSet::apply): writes (or deletes) every change that is not
///   a conflict, and reports each as applied, skipped or failed. An applied
///   change is no longer listed; a skipped or failed one stays.
///
/// On a disk that reads two spellings as one name (macOS's default:
/// `readme.md` and `README.md`; any APFS: `caf\u{e9}` and `cafe\u{301}`), a
/// path is read only as the disk spells it, as the sandbox reads it (see
/// [`SandboxWorkspace`](crate::SandboxWorkspace)): `/README.md` is absent
/// while the disk holds `readme.md`. So `mv readme.md README.md` lists
/// `/readme.md` deleted and `/README.md` added, and `mv lib Lib` lists the
/// same for the directory and each file in it. A change onto a name the disk
/// holds under another spelling, or into a directory it holds so
/// (`Lib/new.rs` beside the disk's `lib`), would land there: it is a
/// conflict, whatever that holds, unless a delete in the list frees the name
/// first (the rename). So the disk holds what the sandbox shows once the
/// list is applied.
///
/// `apply` deletes first, then writes, each in path order, so a rename by
/// case finds its new name free. Each change's real path is read again right
/// before it is put on the disk. One that moved since the scan (the human
/// saved or deleted it), or whose name or directory the disk now holds under
/// another spelling (an earlier write of this apply: the sandbox wrote both
/// `README.md` and `readme.md`; or the rename's delete failed), is skipped
/// as a conflict and stays listed.
///
/// `apply` never writes through a symlink: not one on the real disk on the
/// way to the path ([`OutsideProject`](haest_core::workspace::ApplyFailureCode::OutsideProject)),
/// and not one the sandbox made ([`Symlink`](haest_core::workspace::ApplyFailureCode::Symlink));
/// the human makes that link. A write the disk refuses is
/// [`WriteFailed`](haest_core::workspace::ApplyFailureCode::WriteFailed) and
/// stops nothing else.
///
/// Limit, stated: a directory removed whole (`rm -r dir`) is checked as a
/// directory, not file by file -- an edit the human made INSIDE it meanwhile
/// does not count as a conflict. Where the sandbox makes it again
/// (`rm -r dir && mkdir dir`, `rm -r dist && mv build dist`), each path under
/// it that differs, on the disk or in the sandbox, is listed, and the same
/// holds for it.
///
/// The crate docs have an example of the whole round.
pub struct OverlayChanges {
    book: Arc<Book>,
    overlay: HaestFsOverBashkit,
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
            let on_disk = walk(self.book.disk.as_ref(), &self.book.real(&dir)).await;
            let on_disk = on_disk
                .iter()
                .filter_map(|p| p.strip_prefix(&self.book.root).ok())
                .map(|rel| Path::new("/").join(rel));
            for inside in on_disk.chain(walk(&self.overlay, &dir).await) {
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
            let (now, mine) =
                tokio::join!(self.book.on_disk(&path), state_of(&self.overlay, &path));
            out.push(Found {
                path,
                original: original.unwrap_or(now.state),
                now: now.state,
                held: now.held,
                mine,
            });
        }
        out
    }

    async fn scan(&self) -> Vec<Scanned> {
        changes(self.survey().await)
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
        diff::unified(&c.path, &before, &after)
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
