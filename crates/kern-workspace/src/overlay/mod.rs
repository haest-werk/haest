//! WHAT THE SANDBOX CHANGED, and putting it on the real disk (`/changes`, `/apply`).
//!
//! The sandbox's project overlay keeps every write in memory. [`OverlayChanges`]
//! wraps it so every MUTATION (kern's tools and the simulated shell alike --
//! both go through the overlay) records the path it touched, and what the real
//! file looked like right before: its content hash, "a directory", or "absent".
//!
//! Applying compares three things per path: the real file at first touch, the
//! real file now, and the overlay's version. If the real file moved in between
//! (the human edited it), the path is a CONFLICT and is skipped -- named, never
//! overwritten. Everything else is written (or deleted) on the real disk.
//!
//! Paths here are the overlay's own (`/src/a.ts`); `root` maps them to the disk.
//! Limit, stated: a directory removed whole (`rm -r dir`) is checked as a
//! directory, not file by file -- an edit the human made INSIDE it meanwhile
//! does not count as a conflict.

mod diff;
mod recorder;
mod state;

use std::{
    collections::BTreeMap,
    io,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use async_trait::async_trait;
use kern_core::{
    fs::{Fs, normalize},
    workspace::{ApplyReport, Change, ChangeKind, ChangeSet},
};

use crate::bridge::KernFsOverBashkit;
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

/// What a sandbox changed, and putting it on the real disk.
pub struct OverlayChanges {
    book: Arc<Book>,
    overlay_bk: Arc<dyn bashkit::FileSystem>,
    overlay: KernFsOverBashkit,
}

impl OverlayChanges {
    /// `overlay`: the project overlay; `disk`: the real file system; `root`: the
    /// project dir it overlays.
    pub fn new(
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
            overlay: KernFsOverBashkit::new(overlay.clone()),
            overlay_bk: overlay,
        }
    }

    /// The overlay, recording what every mutation touches before it lands.
    /// Mount THIS, never the bare overlay, or edits go unrecorded.
    pub fn recorder(&self) -> Arc<dyn bashkit::FileSystem> {
        Arc::new(Recorder {
            inner: self.overlay_bk.clone(),
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

    async fn write(&self, c: &Change, path: &Path) -> io::Result<()> {
        let disk = self.book.disk.as_ref();
        let real = self.book.real(path);
        if c.kind == ChangeKind::Deleted {
            return match disk.remove(&real, true).await {
                Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
                _ => Ok(()),
            };
        }
        if self.overlay.stat(path).await?.is_dir() {
            return disk.mkdir(&real, true).await;
        }
        if let Some(parent) = real.parent() {
            disk.mkdir(parent, true).await?;
        }
        disk.write(&real, &self.overlay.read(path).await?).await
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
    /// for a binary file.
    async fn diff(&self, c: &Change) -> String {
        let path = normalize(Path::new(&c.path));
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
    /// applied and what was skipped.
    async fn apply(&self) -> io::Result<ApplyReport> {
        let mut report = ApplyReport::default();
        for s in self.scan().await {
            if s.change.conflict {
                // A file found inside a created directory: remember it, so it is
                // still listed once its directory is applied.
                let mut touched = self.book.touched.lock().unwrap();
                touched.entry(s.path).or_insert(s.original);
                report.skipped.push(s.change);
                continue;
            }
            self.write(&s.change, &s.path).await?;
            self.book.touched.lock().unwrap().remove(&s.path);
            report.applied.push(s.change);
        }
        Ok(report)
    }
}
