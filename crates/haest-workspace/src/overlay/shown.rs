//! WHAT THE SANDBOX STILL SHOWS, which no apply takes away.
//!
//! The apply reads each path as the sandbox spells it (`book.rs`), and that
//! answer has been wrong: a disk's kind kept past a disk swap, a disk that
//! reads both Unicode forms of a name as one where a case swap said it
//! tells names apart, a listing kept past its directory's change. Where it
//! is, the sandbox shows one file on the disk as two (`README.md` and
//! `readme.md`), and the apply's delete or overwrite of one takes the other
//! along: the sandbox goes on showing it, its only copy in memory.
//!
//! So before a change takes away what the disk holds at its path (a delete,
//! a file written over, a directory become a file), two things are asked
//! that the spelling has no say in:
//! - each name on the way is one its directory lists, byte for byte. Else
//!   the disk finds the path under another name, and that one goes;
//! - no other path the sandbox shows that this apply does not add is, now,
//!   the same file, or a directory on its way: `lstat` of both gives the
//!   same device and inode number (exFAT and FAT give an empty file one
//!   number under both spellings, the same for each `lstat`). A path the
//!   disk lists all the way as asked is an entry of its own, a hard link
//!   included, so only one it does not is asked.
//!
//! Either way the change is a conflict: skipped, still listed, and the disk
//! keeps both names as the human left them.

use std::{
    collections::{HashMap, HashSet},
    ffi::OsString,
    os::unix::fs::MetadataExt,
    path::{Component, Path, PathBuf},
    sync::{Arc, Mutex, MutexGuard},
};

/// A device and inode number: one file.
type Id = (u64, u64);

/// The paths the sandbox shows that one apply must leave, and what it read
/// of the disk to tell. One serves a whole apply: the apply's own changes
/// are noted as it makes them ([`Shown::put`]); the human's meanwhile are a
/// race the scan's own re-reads do not close either.
pub(super) struct Shown(Arc<Mutex<State>>);

struct State {
    /// The project directory on the real disk.
    root: PathBuf,
    /// Each path the sandbox shows (an overlay path) but for the ones this
    /// apply adds: those a delete before them frees the name for.
    paths: Vec<PathBuf>,
    /// Each real directory's names, as it lists them; None where it cannot
    /// be read, which leaves the change to the checks after.
    names: HashMap<PathBuf, Option<HashSet<OsString>>>,
    /// The shown paths the disk does not list as asked, each under the id
    /// of what it finds there and of each directory on its way; made at the
    /// first change that takes something away.
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
    /// would take away something the sandbox still shows elsewhere: the
    /// disk finds `path` under another name, or another shown path is the
    /// same file, or under it. Nothing there takes nothing.
    pub(super) async fn would_take(&self, path: &Path) -> bool {
        let (state, path) = (self.0.clone(), path.to_path_buf());
        tokio::task::spawn_blocking(move || lock(&state).would_take(&path))
            .await
            // The check did not finish: nothing says the change is safe.
            .unwrap_or(true)
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
        let hits = self.found().get(&id).cloned().unwrap_or_default();
        hits.iter()
            .any(|shown| shown != path && self.leads_to(shown, id))
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

    /// Whether `shown` (an overlay path), or a directory on its way, is the
    /// file `id` now.
    fn leads_to(&self, shown: &Path, id: Id) -> bool {
        shown
            .ancestors()
            .filter(|a| a.parent().is_some())
            .any(|a| id_of(&self.real(a)) == Some(id))
    }

    /// The shown paths the disk does not list as asked but finds, by the id
    /// of what it finds at each and on its way there.
    fn found(&mut self) -> &HashMap<Id, Vec<PathBuf>> {
        if self.found.is_none() {
            let mut found: HashMap<Id, Vec<PathBuf>> = HashMap::new();
            for shown in std::mem::take(&mut self.paths) {
                if self.listed_as_asked(&shown) {
                    continue;
                }
                for at in shown.ancestors().filter(|a| a.parent().is_some()) {
                    if let Some(id) = id_of(&self.real(at)) {
                        found.entry(id).or_default().push(shown.clone());
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
