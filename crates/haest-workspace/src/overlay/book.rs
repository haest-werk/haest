//! The first-touch ledger, and the real disk read as the sandbox spells a
//! path: where the disk holds a name only under another spelling (`lib` for
//! `/Lib`), the path is absent, and what the disk holds there is kept apart.

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, MutexGuard},
};

use haest_core::fs::{Fs, normalize};

use super::state::{Original, state_of};
use crate::spelling::{Spelled, Spelling};

/// The first-touch ledger the recorder writes and the scan reads.
pub(crate) struct Book {
    touched: Mutex<BTreeMap<PathBuf, Original>>,
    pub(super) disk: Arc<dyn Fs>,
    pub(super) root: PathBuf,
    spelling: Spelling,
}

/// A project path on the real disk, read as the sandbox spells it.
pub(super) struct OnDisk {
    /// What is there: absent where the disk holds the name only under
    /// another spelling.
    pub(super) state: Original,
    /// Where it does: what the disk holds under that other spelling.
    pub(super) held: Option<Held>,
}

/// What the disk holds under another spelling of a path's name.
pub(super) struct Held {
    pub(super) state: Original,
    /// The disk's spelling, as a project path (`/lib/a.rs`); None when it
    /// resolves outside the project.
    pub(super) at: Option<PathBuf>,
}

impl Book {
    /// An empty ledger over the project directory `root` on `disk`.
    pub(super) fn new(disk: Arc<dyn Fs>, root: PathBuf) -> Self {
        Self {
            touched: Mutex::default(),
            disk,
            spelling: Spelling::of(&root),
            root,
        }
    }

    pub(super) fn real(&self, path: &Path) -> PathBuf {
        self.root.join(path.strip_prefix("/").unwrap_or(path))
    }

    /// The first-touch ledger. Its lock is held only to read or write the
    /// map, never across an await or a host's code, so a poisoned one is a
    /// bug here.
    pub(super) fn touched(&self) -> MutexGuard<'_, BTreeMap<PathBuf, Original>> {
        self.touched
            .lock()
            .expect("the overlay's first-touch lock poisoned")
    }

    pub(crate) async fn remember(&self, path: &Path) {
        let path = normalize(path);
        if self.touched().contains_key(&path) {
            return;
        }
        let original = self.on_disk(&path).await.state;
        self.touched().entry(path).or_insert(original);
    }

    pub(super) fn snapshot(&self) -> BTreeMap<PathBuf, Original> {
        self.touched().clone()
    }

    /// `path` (an overlay path) on the real disk, as the sandbox spells it.
    pub(super) async fn on_disk(&self, path: &Path) -> OnDisk {
        let found = state_of(self.disk.as_ref(), &self.real(path)).await;
        if found == Original::Absent {
            return OnDisk {
                state: found,
                held: None,
            };
        }
        match self.spelling.spelled(path).await {
            Spelled::AsAsked => OnDisk {
                state: found,
                held: None,
            },
            Spelled::Other(at) => OnDisk {
                state: Original::Absent,
                held: Some(Held { state: found, at }),
            },
        }
    }
}
