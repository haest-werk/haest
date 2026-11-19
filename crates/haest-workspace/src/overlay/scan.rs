//! From what a scan found at each path to the change it lists: its kind,
//! and whether it is a conflict.

use std::{
    collections::{BTreeMap, BTreeSet},
    ops::Bound,
    path::{Path, PathBuf},
};

use haest_core::workspace::{Change, ChangeKind};

use super::state::Original;

/// One path to compare, as it stands: at first touch, on the real disk now,
/// and in the overlay.
pub(super) struct Found {
    pub(super) path: PathBuf,
    pub(super) original: Original,
    pub(super) now: Original,
    pub(super) mine: Original,
    pub(super) mode: ModeChange,
}

/// What a mode the sandbox set at a path does to the disk's.
#[derive(Debug, Default, Clone, Copy)]
pub(super) struct ModeChange {
    /// Applying changes the disk's mode: with nothing else changed, the
    /// path is listed for it.
    pub(super) differs: bool,
    /// And the human changed the disk's mode since the sandbox set its own:
    /// a conflict, as an edit is.
    pub(super) moved: bool,
}

impl Found {
    /// The overlay differs from the disk here: a change is listed.
    fn changed(&self) -> bool {
        self.now != self.mine || self.mode.differs
    }

    /// The human changed the disk here since the sandbox first read it (an
    /// edit, a delete, a mode): applying would go over it.
    fn edited(&self) -> bool {
        self.original != self.now || self.mode.moved
    }

    /// The change the overlay makes here; None when it holds what the disk
    /// does.
    fn change(self) -> Option<Scanned> {
        if !self.changed() {
            return None;
        }
        let kind = match (self.mine, self.now) {
            (Original::Absent, _) => ChangeKind::Deleted,
            (_, Original::Absent) => ChangeKind::Added,
            _ => ChangeKind::Modified,
        };
        Some(Scanned {
            change: Change::new(self.path.to_string_lossy(), kind).with_conflict(self.edited()),
            path: self.path,
            original: self.original,
            now: self.now,
            mine: self.mine,
        })
    }
}

/// The paths of `paths` under `dir`, `dir` itself not counted: in path
/// order, they come right after it.
pub(super) fn under<'a, V>(
    paths: &'a BTreeMap<PathBuf, V>,
    dir: &'a Path,
) -> impl Iterator<Item = (&'a PathBuf, &'a V)> {
    paths
        .range::<Path, _>((Bound::Excluded(dir), Bound::Unbounded))
        .take_while(move |(p, _)| p.starts_with(dir))
}

/// Whether `paths` holds one under `dir`, `dir` itself not counted.
fn any_under(paths: &BTreeSet<PathBuf>, dir: &Path) -> bool {
    paths
        .range::<Path, _>((Bound::Excluded(dir), Bound::Unbounded))
        .next()
        .is_some_and(|p| p.starts_with(dir))
}

/// The changes among `found`, in its order, each a conflict where the
/// human changed its path since the sandbox first read it.
pub(super) fn changes(found: Vec<Found>) -> Vec<Scanned> {
    let mut scanned: Vec<Scanned> = found.into_iter().filter_map(Found::change).collect();
    // A change that removes a directory takes all under it along: a
    // conflict there would go with it while the report says it was
    // skipped, and the human's version with it. It waits too.
    let waiting: BTreeSet<PathBuf> = scanned
        .iter()
        .filter(|s| s.change.conflict)
        .map(|s| s.path.clone())
        .collect();
    for s in &mut scanned {
        if s.removes_tree() && any_under(&waiting, &s.path) {
            s.change.conflict = true;
        }
    }
    scanned
}

/// One change, and the states it was found from.
pub(super) struct Scanned {
    pub(super) change: Change,
    pub(super) path: PathBuf,
    pub(super) original: Original,
    /// The real path when scanned: what applying expects to find there.
    pub(super) now: Original,
    /// The overlay's version when scanned.
    pub(super) mine: Original,
}

impl Scanned {
    /// Applied, it removes a directory the disk held when scanned (a
    /// delete, or a directory become a file), with all under it.
    pub(super) fn removes_tree(&self) -> bool {
        self.now == Original::Dir && self.mine != Original::Dir
    }
}
