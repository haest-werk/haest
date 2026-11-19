//! The first-touch ledger: what the real disk held at each path the
//! sandbox touched, read through the system as any program reads it.

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, MutexGuard},
};

use haest_core::fs::{Fs, normalize};
use tokio::sync::RwLock;

use super::{
    modes::{self, SetMode, SetModes},
    state::{Original, state_of, walk},
};

/// The first-touch ledger the recorder writes and the scan reads.
pub(crate) struct Book {
    touched: Mutex<BTreeMap<PathBuf, Original>>,
    /// Directories the sandbox removed whole, moved away or moved a tree
    /// onto: the overlay hides all the disk held under each then, and
    /// records only the directory itself.
    wiped: Mutex<Wiped>,
    /// The modes the sandbox set (`chmod`).
    pub(super) modes: SetModes,
    /// Each move and copy the sandbox made, `(from, to)`, in the order it
    /// made them: where what the disk holds went in the sandbox, so the
    /// apply takes nothing away while a write that carries it waits.
    carried: Mutex<Vec<(PathBuf, PathBuf)>>,
    /// Held shared by each change the recorder passes on, and alone by an
    /// apply: a sandbox write waits for the apply in progress, and the
    /// apply for the writes already under way, so none lands between the
    /// apply's read of a path and its reading the disk there again or
    /// forgetting it.
    pub(super) gate: RwLock<()>,
    pub(super) disk: Arc<dyn Fs>,
    pub(super) root: PathBuf,
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

impl Book {
    /// An empty ledger over the project directory `root` on `disk`.
    pub(super) fn new(disk: Arc<dyn Fs>, root: PathBuf) -> Self {
        Self {
            touched: Mutex::default(),
            wiped: Mutex::default(),
            modes: SetModes::default(),
            carried: Mutex::default(),
            gate: RwLock::new(()),
            disk,
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
        self.wiped()
            .dirs
            .iter()
            .map(|(dir, &count)| Wipe {
                dir: dir.clone(),
                count,
            })
            .collect()
    }

    /// Forget each of `wipes`, where the sandbox has not wiped its directory
    /// again since: what a wipe made later hid is still to compare.
    pub(super) fn forget_wiped<'a>(&self, wipes: impl IntoIterator<Item = &'a Wipe>) {
        let mut book = self.wiped();
        for wipe in wipes {
            if book.dirs.get(&wipe.dir) == Some(&wipe.count) {
                book.dirs.remove(&wipe.dir);
            }
        }
    }

    /// The moves and copies so far. Their lock is held only to read or
    /// write them, like the ledger's.
    fn carried_lock(&self) -> MutexGuard<'_, Vec<(PathBuf, PathBuf)>> {
        self.carried
            .lock()
            .expect("the overlay's moves lock poisoned")
    }

    /// The sandbox moved or copied `from` to `to`.
    pub(crate) fn carry(&self, from: &Path, to: &Path) {
        self.carried_lock().push((normalize(from), normalize(to)));
    }

    /// Each move and copy the sandbox made, in the order it made them.
    pub(super) fn carried(&self) -> Vec<(PathBuf, PathBuf)> {
        self.carried_lock().clone()
    }

    /// Forget each move and copy that neither came from nor went to a path
    /// still in the ledger, or one above or under such a path: nothing
    /// still to apply depends on it.
    pub(super) fn forget_carried(&self) {
        let touched = self.snapshot();
        let near = |p: &Path| touched.keys().any(|t| t.starts_with(p) || p.starts_with(t));
        self.carried_lock()
            .retain(|(from, to)| near(from) || near(to));
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

    /// Whether the sandbox's mode at `path` is the path's own, which `mv`
    /// and `cp` take along: one the sandbox set, or the disk's, where the
    /// disk holds the path. A path only the sandbox made
    /// has bashkit's default mode, where bash gives the umask's: applying
    /// gives it the umask's. (One it made again after removing the disk's
    /// counts as the disk's: it lands 0644 at most. A directory the disk
    /// holds that the sandbox changed something under counts as well,
    /// though the sandbox shows it 0755, not the disk's mode.)
    pub(crate) async fn own_mode(&self, path: &Path) -> bool {
        self.modes.get(&normalize(path)).is_some()
            || modes::on_disk(&self.real(path)).await.is_some()
    }

    /// A move or a copy brought `mode` to `path`, as the path's own. Kept as
    /// a mode the sandbox set, so applying carries it, unless the disk has
    /// that mode there already: then the path is an edit, and a mode the
    /// human gives it later is theirs, as after any edit.
    pub(crate) async fn bring_mode(&self, path: &Path, mode: u32) {
        if self.disk_mode(path).await == Some(mode & modes::PERMISSIONS) {
            self.modes.forget(&normalize(path));
        } else {
            self.set_mode(path, mode).await;
        }
    }

    /// The mode of `path` (an overlay path) on the real disk; None where
    /// nothing is there.
    pub(super) async fn disk_mode(&self, path: &Path) -> Option<u32> {
        modes::on_disk(&self.real(path)).await
    }

    /// What the real disk holds at `path` (an overlay path).
    pub(super) async fn state(&self, path: &Path) -> Original {
        state_of(self.disk.as_ref(), &self.real(path)).await
    }
}
