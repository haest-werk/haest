//! WHAT THE SANDBOX SHOWS, which no apply takes away or writes past.
//!
//! The sandbox reads the disk through the system, and keeps what it writes
//! under the name it wrote it by. Where the system reads two names as one
//! file (`README.md` and `readme.md` on macOS's default disk, `café` in
//! either Unicode form on any APFS, exFAT, FAT, vfat), the sandbox can show
//! that one file as two, and an apply's delete or overwrite of one would
//! take the other along: the sandbox would go on showing it, its only copy
//! in memory.
//!
//! The same holds for a directory: the sandbox's own `Lib` (`mv lib Lib`
//! moves `lib` into it, as bashkit's `mv` does with a directory that is
//! there) shows the files of the disk's `lib` read through, and a delete
//! under `lib` takes them from `Lib` too. A write into that `Lib` lands in
//! the disk's `lib`, where the sandbox does not show it: `mv lib Lib`
//! wrote `lib/lib/a.rs`.
//!
//! So before a change takes away what the disk holds at its path (a delete,
//! a file written over, a directory become a file), two things are asked
//! of the disk, with `read_dir` and `symlink_metadata` alone:
//! - each name on the way is one its directory lists, byte for byte. Else
//!   the system finds the path under another name, and that one goes;
//! - no other path the sandbox shows, of those it changed and each
//!   directory on the way to them, is now the same file as the path or a
//!   directory on its way: `lstat` of both gives the same device and inode
//!   number (exFAT and FAT give an empty file one number under both names,
//!   the same for each `lstat`). A path the disk lists all the way as asked
//!   is an entry of its own, a hard link included, so only one it does not
//!   is asked.
//!
//! And before a write, each name on its way the disk holds is one its
//! directory lists, and so is the path's own where the disk holds it.
//!
//! Each way the change is a conflict: skipped, still listed, and the disk
//! keeps both names as the human left them. A path the sandbox reads
//! through and never touched is not asked: a change that takes it is one
//! the list names.

use std::{
    collections::{HashMap, HashSet},
    ffi::OsString,
    os::unix::fs::MetadataExt,
    path::{Component, Path, PathBuf},
    sync::{Arc, Mutex, MutexGuard},
};

/// A device and inode number: one file.
type Id = (u64, u64);

/// The paths the sandbox shows, which no change may take along, and what
/// was read of the disk to tell. One serves an apply: its own changes are
/// noted as it makes them ([`Shown::put`]), and the disk is read again
/// between its phases ([`Shown::refresh`]), so the deletes see what its
/// writes put there; the human's changes meanwhile are a race the apply's
/// own re-reads do not close either.
pub(super) struct Shown(Arc<Mutex<State>>);

struct State {
    /// The project directory on the real disk.
    root: PathBuf,
    /// Each path the sandbox changed and still shows, and each directory on
    /// the way to one it changed that it still shows (overlay paths).
    paths: Vec<PathBuf>,
    /// Each real directory's names, as it lists them; None where it cannot
    /// be read, which leaves the change to the checks after.
    names: HashMap<PathBuf, Option<HashSet<OsString>>>,
    /// For each shown path the disk does not list as asked, it and each
    /// directory on its way, under the id of what the disk finds there;
    /// made at the first change that takes something away.
    found: Option<HashMap<Id, Vec<PathBuf>>>,
}

impl Shown {
    /// For an apply over the project `root`, which must leave `paths`.
    pub(super) fn new(root: &Path, paths: Vec<PathBuf>) -> Self {
        Self(Arc::new(Mutex::new(State {
            root: root.to_path_buf(),
            paths,
            names: HashMap::new(),
            found: None,
        })))
    }

    /// Whether putting a change at `path` (an overlay path) on the disk
    /// would take away something else: the disk finds `path` under another
    /// name, or another path the sandbox shows is the same file as `path`
    /// or as a directory on its way, or under it. Nothing there takes
    /// nothing.
    pub(super) async fn would_take(&self, path: &Path) -> bool {
        let (state, path) = (self.0.clone(), path.to_path_buf());
        tokio::task::spawn_blocking(move || lock(&state).would_take(&path))
            .await
            // The check did not finish: nothing says the change is safe.
            .unwrap_or(true)
    }

    /// Whether a write at `path` (an overlay path) would land under another
    /// name: a directory on its way, or the path itself where `there`, is
    /// on the disk but not listed by that name. The sandbox shows it apart
    /// from what the disk holds there (its own `Lib`, beside the disk's
    /// `lib`), so the write would not land where the sandbox shows it.
    pub(super) async fn lands_elsewhere(&self, path: &Path, there: bool) -> bool {
        let (state, path) = (self.0.clone(), path.to_path_buf());
        tokio::task::spawn_blocking(move || lock(&state).lands_elsewhere(&path, there))
            .await
            .unwrap_or(true)
    }

    /// Read the disk again from here: what this apply wrote is on it, the
    /// sandbox's own files at their names.
    pub(super) fn refresh(&self) {
        let mut state = lock(&self.0);
        state.names.clear();
        state.found = None;
    }

    /// The change at `path` is on the disk now: a delete took its name, or
    /// a write made it (and the directories on its way) where it was not.
    pub(super) fn put(&self, path: &Path, deleted: bool) {
        let mut state = lock(&self.0);
        if deleted {
            state.removed(path);
        } else {
            state.added(path);
        }
    }
}

/// Held for one check or note, on a blocking thread or briefly, never
/// across an await, so a poisoned one is a bug here.
fn lock(state: &Mutex<State>) -> MutexGuard<'_, State> {
    state.lock().expect("the apply's shown-paths lock poisoned")
}

/// Each name of `path`, an overlay path.
fn names_of(path: &Path) -> impl Iterator<Item = &std::ffi::OsStr> {
    path.components().filter_map(|c| match c {
        Component::Normal(name) => Some(name),
        _ => None,
    })
}

fn id_of(real: &Path) -> Option<Id> {
    let m = std::fs::symlink_metadata(real).ok()?;
    Some((m.dev(), m.ino()))
}

impl State {
    fn real(&self, path: &Path) -> PathBuf {
        self.root.join(path.strip_prefix("/").unwrap_or(path))
    }

    fn would_take(&mut self, path: &Path) -> bool {
        let Some(id) = id_of(&self.real(path)) else {
            return false;
        };
        if !self.listed_as_asked(path) {
            return true;
        }
        if self.found().is_empty() {
            return false;
        }
        // The path, then each directory on its way: another shown path that
        // is the same file shows what the change takes, there or under it.
        for at in path.ancestors().filter(|a| a.parent().is_some()) {
            let at_id = if at == path {
                Some(id)
            } else {
                id_of(&self.real(at))
            };
            let Some(at_id) = at_id else {
                continue;
            };
            let hits = self.found().get(&at_id).cloned().unwrap_or_default();
            if hits
                .iter()
                .any(|shown| shown != at && id_of(&self.real(shown)) == Some(at_id))
            {
                return true;
            }
        }
        false
    }

    fn lands_elsewhere(&mut self, path: &Path, there: bool) -> bool {
        let mut dir = self.root.clone();
        let mut names = names_of(path).peekable();
        while let Some(name) = names.next() {
            if names.peek().is_none() && !there {
                // Not on the disk by any name: the write makes it.
                return false;
            }
            let listed = self.names(&dir).is_none_or(|names| names.contains(name));
            dir.push(name);
            if !listed {
                // Not there: neither is anything under it, and the write
                // makes the way.
                return std::fs::symlink_metadata(&dir).is_ok();
            }
        }
        false
    }

    /// Whether each name of `path` is one its directory lists as written.
    fn listed_as_asked(&mut self, path: &Path) -> bool {
        let mut dir = self.root.clone();
        for name in names_of(path) {
            if self.names(&dir).is_some_and(|names| !names.contains(name)) {
                return false;
            }
            dir.push(name);
        }
        true
    }

    /// The names the real directory `dir` lists, read once.
    fn names(&mut self, dir: &Path) -> Option<&HashSet<OsString>> {
        if !self.names.contains_key(dir) {
            let read = std::fs::read_dir(dir).ok().and_then(|entries| {
                entries
                    .map(|e| e.map(|e| e.file_name()))
                    .collect::<Result<_, _>>()
                    .ok()
            });
            self.names.insert(dir.to_path_buf(), read);
        }
        self.names.get(dir)?.as_ref()
    }

    /// Each shown path the disk does not list as asked but finds, and each
    /// directory on its way, by the id of what the disk finds there.
    fn found(&mut self) -> &HashMap<Id, Vec<PathBuf>> {
        if self.found.is_none() {
            let mut found: HashMap<Id, Vec<PathBuf>> = HashMap::new();
            let mut indexed: HashSet<PathBuf> = HashSet::new();
            for shown in self.paths.clone() {
                if self.listed_as_asked(&shown) {
                    continue;
                }
                for at in shown.ancestors().filter(|a| a.parent().is_some()) {
                    if !indexed.insert(at.to_path_buf()) {
                        // Indexed with what is above it.
                        break;
                    }
                    if let Some(id) = id_of(&self.real(at)) {
                        found.entry(id).or_default().push(at.to_path_buf());
                    }
                }
            }
            self.found = Some(found);
        }
        self.found.get_or_insert_default()
    }

    /// A delete took `path`'s name, and all under it.
    fn removed(&mut self, path: &Path) {
        let real = self.real(path);
        if let (Some(dir), Some(name)) = (real.parent(), real.file_name())
            && let Some(Some(names)) = self.names.get_mut(dir)
        {
            names.remove(name);
        }
        self.names.retain(|dir, _| !dir.starts_with(&real));
    }

    /// A write made `path`, and each directory on its way it was missing.
    fn added(&mut self, path: &Path) {
        let mut dir = self.root.clone();
        for name in names_of(path) {
            if let Some(Some(names)) = self.names.get_mut(&dir) {
                names.insert(name.to_os_string());
            }
            dir.push(name);
        }
    }
}
