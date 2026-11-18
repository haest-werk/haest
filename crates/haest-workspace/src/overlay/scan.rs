//! From what a scan found at each path to the change it lists: its kind,
//! and whether it is a conflict.

use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
};

use haest_core::workspace::{Change, ChangeKind};

use super::{book::Held, state::Original};

/// One path to compare, as it stands: at first touch, on the real disk now
/// (both as the sandbox spells the path), and in the overlay.
pub(super) struct Found {
    pub(super) path: PathBuf,
    pub(super) original: Original,
    pub(super) now: Original,
    /// Where the disk holds the path's name, or a directory on its way,
    /// under another spelling.
    pub(super) held: Option<Held>,
    pub(super) mine: Original,
}

impl Found {
    /// A delete that is not a conflict: applying frees its name.
    fn frees(&self) -> bool {
        self.mine == Original::Absent && self.now != Original::Absent && self.original == self.now
    }

    /// The disk's spelling of the name this change would land on, when no
    /// delete in `freed` frees it.
    fn lands_on(&self, freed: &BTreeSet<PathBuf>) -> Option<&Held> {
        self.held
            .as_ref()
            .filter(|h| !h.at.as_deref().is_some_and(|at| under_any(at, freed)))
    }

    /// The change the overlay makes here; None when it holds what the disk
    /// does. `freed` are the names this change set's deletes free.
    fn change(self, freed: &BTreeSet<PathBuf>) -> Option<Scanned> {
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
        let keeps = self.lands_on(freed).map(|h| h.at.clone());
        Some(Scanned {
            change: Change::new(self.path.to_string_lossy(), kind)
                .with_conflict(self.original != self.now || keeps.is_some()),
            keeps: keeps.flatten(),
            frees: false,
            path: self.path,
            original: self.original,
            now: self.now,
            mine: self.mine,
        })
    }
}

/// Whether `path` is one of `dirs` or under one.
fn under_any(path: &Path, dirs: &BTreeSet<PathBuf>) -> bool {
    path.ancestors().any(|a| dirs.contains(a))
}

/// The changes among `found`, in its order.
pub(super) fn changes(found: Vec<Found>) -> Vec<Scanned> {
    let freed: BTreeSet<PathBuf> = found
        .iter()
        .filter(|f| f.frees())
        .map(|f| f.path.clone())
        .collect();
    // The deletes a write needs: each frees the name the write lands on.
    let needed: BTreeSet<PathBuf> = found
        .iter()
        .filter_map(|f| f.held.as_ref()?.at.as_deref())
        .flat_map(Path::ancestors)
        .filter(|a| freed.contains(*a))
        .map(Path::to_path_buf)
        .collect();
    let mut scanned: Vec<Scanned> = found.into_iter().filter_map(|f| f.change(&freed)).collect();
    // A write that cannot land leaves its name as the disk holds it, so
    // what would take something away there (a delete, an overwrite, a
    // directory become a file) waits with it, a conflict too: else `mv lib
    // Lib && mkdir lib` deleted lib's files on a disk that reads both names
    // as one, and wrote none of Lib's. A file added there takes nothing.
    let kept: BTreeSet<PathBuf> = scanned.iter().filter_map(|s| s.keeps.clone()).collect();
    for s in &mut scanned {
        s.frees = needed.contains(&s.path);
        if s.change.kind != ChangeKind::Added && under_any(&s.path, &kept) {
            s.change.conflict = true;
        }
    }
    scanned
}

/// One change, and the states it was found from.
pub(super) struct Scanned {
    pub(super) change: Change,
    /// A write that would land on a name the disk holds under another
    /// spelling: that name, as the disk spells it.
    keeps: Option<PathBuf>,
    /// A delete that frees the name a write in this change set lands on.
    pub(super) frees: bool,
    pub(super) path: PathBuf,
    pub(super) original: Original,
    /// The real path when scanned: what applying expects to find there.
    pub(super) now: Original,
    /// The overlay's version when scanned.
    pub(super) mine: Original,
}
