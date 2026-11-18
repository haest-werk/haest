//! The first-touch ledger, and the real disk read as the sandbox spells a
//! path: where the disk holds a name only under another spelling (`lib` for
//! `/Lib`), the path is absent, and where a write to it would land instead
//! is kept apart. A path that is not there lands in the nearest directory
//! above it that is (`/Lib/new.rs` in `lib`), so that directory's spelling
//! counts too.

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, MutexGuard},
};

use haest_core::fs::{Fs, normalize};

use super::{
    modes::{self, SetMode, SetModes},
    state::{Original, state_of, walk},
};
use crate::spelling::{Spelled, Spelling, Ways};

/// The first-touch ledger the recorder writes and the scan reads.
pub(crate) struct Book {
    touched: Mutex<BTreeMap<PathBuf, Original>>,
    /// Directories the sandbox removed whole, moved away or moved a tree
    /// onto: the overlay hides all the disk held under each then, and
    /// records only the directory itself.
    wiped: Mutex<Wiped>,
    /// The modes the sandbox set (`chmod`).
    pub(super) modes: SetModes,
    pub(super) disk: Arc<dyn Fs>,
    pub(super) root: PathBuf,
    spelling: Spelling,
}

/// The directories wiped, each with the count of the last wipe of it.
#[derive(Debug, Default)]
struct Wiped {
    dirs: BTreeMap<PathBuf, u64>,
    /// Wipes so far.
    count: u64,
}

/// One wipe of a directory: forgotten only if it is still the last one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Wipe {
    pub(super) dir: PathBuf,
    count: u64,
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
            modes: SetModes::default(),
            disk,
            spelling: Spelling::of(&root),
            root,
        }
    }

    pub(super) fn real(&self, path: &Path) -> PathBuf {
        self.root.join(path.strip_prefix("/").unwrap_or(path))
    }

    /// Every path the disk holds under `dir` (an overlay path), as overlay
    /// paths.
    pub(super) async fn tree(&self, dir: &Path) -> Vec<PathBuf> {
        let real = walk(self.disk.as_ref(), &self.real(dir)).await;
        real.iter()
            .filter_map(|p| p.strip_prefix(&self.root).ok())
            .map(|rel| Path::new("/").join(rel))
            .collect()
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
    /// write them, like the ledger's.
    fn wiped(&self) -> MutexGuard<'_, Wiped> {
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
            let mut wiped = self.wiped();
            wiped.count += 1;
            let count = wiped.count;
            wiped.dirs.insert(path, count);
        }
    }

    /// Every directory wiped so far, in path order: one before those under it.
    pub(super) fn wiped_dirs(&self) -> Vec<PathBuf> {
        self.wiped().dirs.keys().cloned().collect()
    }

    /// The last wipe of each directory wiped so far, in path order.
    pub(super) fn wipes(&self) -> Vec<Wipe> {
        let wiped = self.wiped();
        let wipes = wiped.dirs.iter().map(|(dir, &count)| Wipe {
            dir: dir.clone(),
            count,
        });
        wipes.collect()
    }

    /// Forget each of `wipes`, where the sandbox has not wiped its directory
    /// again since: what a wipe made later hid is still to compare.
    pub(super) fn forget_wiped<'a>(&self, wipes: impl IntoIterator<Item = &'a Wipe>) {
        let mut wiped = self.wiped();
        for wipe in wipes {
            if wiped.dirs.get(&wipe.dir) == Some(&wipe.count) {
                wiped.dirs.remove(&wipe.dir);
            }
        }
    }

    /// The sandbox set `path`'s mode to `mode`. The disk's mode there is
    /// kept from the first time it did.
    pub(crate) async fn set_mode(&self, path: &Path, mode: u32) {
        let path = normalize(path);
        let disk = match self.modes.get(&path) {
            Some(set) => set.disk,
            None => self.disk_mode(&path).await,
        };
        self.modes.insert(path, SetMode { mode, disk });
    }

    /// The mode of `path` (an overlay path) on the real disk, as the sandbox
    /// spells it: None where the disk holds it only under another spelling.
    pub(super) async fn disk_mode(&self, path: &Path) -> Option<u32> {
        let mode = modes::on_disk(&self.real(path)).await?;
        match self.spelling.spelled(path).await {
            Spelled::AsAsked => Some(mode),
            Spelled::Other(_) => None,
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
    /// and where a write to it would land instead. `ways` keeps how the disk
    /// spells the directories above a missing path, for one scan.
    pub(super) async fn on_disk(&self, path: &Path, ways: &mut Ways) -> OnDisk {
        let found = state_of(self.disk.as_ref(), &self.real(path)).await;
        let spelled = if found == Original::Absent {
            self.spelling.spelled_way(path, ways).await
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
