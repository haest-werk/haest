//! The first-touch ledger, and the real disk read as the sandbox spells a
//! path: where the disk holds a name only under another spelling (`lib` for
//! `/Lib`), the path is absent, and where a write to it would land instead
//! is kept apart. A path that is not there lands in the nearest directory
//! above it that is (`/Lib/new.rs` in `lib`), so that directory's spelling
//! counts too.

use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
    sync::{Arc, Mutex, MutexGuard},
};

use haest_core::fs::{Fs, normalize};

use super::state::{Original, state_of};
use crate::spelling::{Spelled, Spelling};

/// The first-touch ledger the recorder writes and the scan reads.
pub(crate) struct Book {
    touched: Mutex<BTreeMap<PathBuf, Original>>,
    /// Directories the sandbox removed whole, moved away or moved a tree
    /// onto: the overlay hides all the disk held under each then, and
    /// records only the directory itself.
    wiped: Mutex<BTreeSet<PathBuf>>,
    pub(super) disk: Arc<dyn Fs>,
    pub(super) root: PathBuf,
    spelling: Spelling,
}

/// A project path on the real disk, read as the sandbox spells it.
pub(super) struct OnDisk {
    /// What is there: absent where the disk holds the name only under
    /// another spelling.
    pub(super) state: Original,
    /// Where it does, or holds a directory on its way so: a write to the
    /// path lands there, not where the sandbox shows it.
    pub(super) held: Option<Held>,
}

/// Where the disk holds a path's name, or a directory on its way, under
/// another spelling.
pub(super) struct Held {
    /// The disk's own spelling of the first name on the way it spells
    /// differently, as a project path (`/lib` for `/Lib/a.rs`): a delete of
    /// it frees the way. None when the path resolves outside the project.
    pub(super) at: Option<PathBuf>,
}

impl Held {
    /// `asked` (an overlay path) is held at `found`, the disk's own spelling
    /// of it or of a directory above it.
    fn new(asked: &Path, found: Option<PathBuf>) -> Self {
        let at = found.map(|found| {
            let mut asked = asked.iter();
            let mut at = PathBuf::new();
            for name in &found {
                at.push(name);
                if asked.next() != Some(name) {
                    break;
                }
            }
            at
        });
        Self { at }
    }
}

impl Book {
    /// An empty ledger over the project directory `root` on `disk`.
    pub(super) fn new(disk: Arc<dyn Fs>, root: PathBuf) -> Self {
        Self {
            touched: Mutex::default(),
            wiped: Mutex::default(),
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
        let original = self.state(&path).await;
        self.touched().entry(path).or_insert(original);
    }

    pub(super) fn snapshot(&self) -> BTreeMap<PathBuf, Original> {
        self.touched().clone()
    }

    /// The directories wiped so far. Their lock is held only to read or
    /// write the set, like the ledger's.
    fn wiped(&self) -> MutexGuard<'_, BTreeSet<PathBuf>> {
        self.wiped
            .lock()
            .expect("the overlay's wiped lock poisoned")
    }

    /// The sandbox removed `path` whole (`rm -r`), moved it away, or moved a
    /// tree onto it. Kept only where the disk held a directory at first
    /// touch: under anything else, the overlay hides nothing of the disk's.
    pub(crate) fn wipe(&self, path: &Path) {
        let path = normalize(path);
        if self.touched().get(&path) == Some(&Original::Dir) {
            self.wiped().insert(path);
        }
    }

    /// Every directory wiped so far, in path order: one before those under it.
    pub(super) fn wiped_dirs(&self) -> Vec<PathBuf> {
        self.wiped().iter().cloned().collect()
    }

    /// Forget each of `dirs` as wiped.
    pub(super) fn forget_wiped<'a>(&self, dirs: impl IntoIterator<Item = &'a PathBuf>) {
        let mut wiped = self.wiped();
        for dir in dirs {
            wiped.remove(dir);
        }
    }

    /// What the sandbox reads at `path` (an overlay path) on the real disk:
    /// absent where the disk holds it only under another spelling.
    async fn state(&self, path: &Path) -> Original {
        let found = state_of(self.disk.as_ref(), &self.real(path)).await;
        if found == Original::Absent {
            return found;
        }
        match self.spelling.spelled(path).await {
            Spelled::AsAsked => found,
            Spelled::Other(_) => Original::Absent,
        }
    }

    /// `path` (an overlay path) on the real disk, as the sandbox spells it,
    /// and where a write to it would land instead.
    pub(super) async fn on_disk(&self, path: &Path) -> OnDisk {
        let found = state_of(self.disk.as_ref(), &self.real(path)).await;
        let spelled = if found == Original::Absent {
            self.spelling.spelled_way(path).await
        } else {
            self.spelling.spelled(path).await
        };
        match spelled {
            Spelled::AsAsked => OnDisk {
                state: found,
                held: None,
            },
            Spelled::Other(at) => OnDisk {
                state: Original::Absent,
                held: Some(Held::new(path, at)),
            },
        }
    }
}
