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
//! overwritten. Everything else is written (or deleted) on the real disk,
//! unless `apply.rs` refuses it: a symlink on the real disk on its way, or a
//! symlink the sandbox made. A refused change is reported as failed with its
//! code, and stays listed.
//!
//! Paths here are the overlay's own (`/src/a.ts`); `root` maps them to the disk.
//! A symlink is compared as a link (`state.rs`), never as what it points to.
//! Limit, stated: a directory removed whole (`rm -r dir`) is checked as a
//! directory, not file by file -- an edit the human made INSIDE it meanwhile
//! does not count as a conflict.

mod apply;
mod diff;
mod links;
mod recorder;
mod state;

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use async_trait::async_trait;
use haest_core::{
    fs::{Fs, normalize},
    workspace::{ApplyReport, Change, ChangeKind, ChangeSet, FailedChange},
};

use crate::bridge::HaestFsOverBashkit;
use recorder::Recorder;
use state::{Original, state_of, walk};

/// The first-touch ledger the recorder writes and the scan reads.
pub(crate) struct Book {
    touched: Mutex<BTreeMap<PathBuf, Original>>,
    disk: Arc<dyn Fs>,
    root: PathBuf,
}

impl Book {
    fn real(&self, path: &Path) -> PathBuf {
        self.root.join(path.strip_prefix("/").unwrap_or(path))
    }

    pub(crate) async fn remember(&self, path: &Path) {
        let path = normalize(path);
        if self.touched.lock().unwrap().contains_key(&path) {
            return;
        }
        let original = state_of(self.disk.as_ref(), &self.real(&path)).await;
        self.touched.lock().unwrap().entry(path).or_insert(original);
    }

    fn snapshot(&self) -> BTreeMap<PathBuf, Original> {
        self.touched.lock().unwrap().clone()
    }
}

struct Scanned {
    change: Change,
    path: PathBuf,
    original: Original,
}

/// What a sandbox changed in the project, and putting it on the real disk
/// (haest-core's [`ChangeSet`]).
///
/// [`SandboxWorkspace::open`](crate::SandboxWorkspace::open) builds one over
/// its project overlay; a host reaches it through
/// [`Workspace::changes`](haest_core::workspace::Workspace::changes), or typed
/// through [`SandboxWorkspace::overlay_changes`](crate::SandboxWorkspace::overlay_changes).
///
/// Every mutation to the project -- haest's tools and the simulated shell
/// alike -- first records the path it touched
/// and what the real file looked like right then: a hash of its content, "a
/// directory", "a symlink to X", or "absent". From that:
///
/// - [`list`](ChangeSet::list): every touched path whose overlay version
///   differs from the real disk, by its path inside the project (`/src/a.ts`),
///   a directory before its files. A path whose real file changed since the
///   first touch (the human edited it) is a [`conflict`](haest_core::workspace::Change::conflict).
/// - [`diff`](ChangeSet::diff): a unified diff of one change.
/// - [`apply`](ChangeSet::apply): writes (or deletes) every change that is not
///   a conflict, and reports each as applied, skipped or failed. An applied
///   change is no longer listed; a skipped or failed one stays.
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
/// does not count as a conflict.
///
/// The crate docs have an example of the whole round.
pub struct OverlayChanges {
    book: Arc<Book>,
    overlay: HaestFsOverBashkit,
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
            book: Arc::new(Book {
                touched: Mutex::default(),
                disk,
                root: root.into(),
            }),
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

    /// Every path to compare: the touched ones, plus everything inside a directory
    /// the sandbox CREATED (`mv d1 d2`, `cp -r a b` record only `d2` / `b`, and
    /// applying that as a bare mkdir would leave it empty). Nothing under it existed
    /// on disk at first touch, so each file inside starts out absent.
    async fn candidates(&self) -> BTreeMap<PathBuf, Original> {
        let touched = self.book.snapshot();
        let mut all = touched.clone();
        for (path, original) in &touched {
            if *original == Original::Dir || state_of(&self.overlay, path).await != Original::Dir {
                continue;
            }
            for inside in walk(&self.overlay, path).await {
                all.entry(inside).or_insert(Original::Absent);
            }
        }
        all
    }

    async fn scan(&self) -> Vec<Scanned> {
        let mut out = Vec::new();
        for (path, original) in self.candidates().await {
            let real = self.book.real(&path);
            let (now, mine) = tokio::join!(
                state_of(self.book.disk.as_ref(), &real),
                state_of(&self.overlay, &path)
            );
            if now == mine {
                continue;
            }
            let kind = match (mine, now) {
                (Original::Absent, _) => ChangeKind::Deleted,
                (_, Original::Absent) => ChangeKind::Added,
                _ => ChangeKind::Modified,
            };
            out.push(Scanned {
                change: Change {
                    path: path.to_string_lossy().into_owned(),
                    kind,
                    conflict: original != now,
                },
                path,
                original,
            });
        }
        // Path order: a directory before its files.
        out.sort_by(|a, b| a.path.cmp(&b.path));
        out
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

    /// Write every non-conflicting change to the real disk. Returns what was
    /// applied, what was skipped (a conflict), and what failed, each with why
    /// (a symlink, a disk error). One that fails stops nothing else, and stays
    /// listed like a conflict.
    async fn apply(&self) -> ApplyReport {
        let mut report = ApplyReport::default();
        for s in self.scan().await {
            let refused = if s.change.conflict {
                None
            } else {
                match self.put(s.change.kind, &s.path).await {
                    Ok(()) => {
                        self.book.touched.lock().unwrap().remove(&s.path);
                        report.applied.push(s.change);
                        continue;
                    }
                    Err(refusal) => Some(refusal),
                }
            };
            // A file found inside a created directory: remember it, so it is
            // still listed once its directory is applied.
            let mut touched = self.book.touched.lock().unwrap();
            touched.entry(s.path).or_insert(s.original);
            match refused {
                Some((code, reason)) => report.failed.push(FailedChange {
                    change: s.change,
                    code,
                    reason,
                }),
                None => report.skipped.push(s.change),
            }
        }
        report
    }
}
