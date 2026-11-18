//! From what a scan found at each path to the change it lists: its kind,
//! and whether it is a conflict.

use std::path::PathBuf;

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

    /// The change the overlay makes here; None when it holds what the disk
    /// does. `freed` are the names this change set's deletes free.
    fn change(self, freed: &[PathBuf]) -> Option<Scanned> {
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
        let lands_on_another = self.held.is_some_and(|h| {
            !h.at
                .is_some_and(|at| freed.iter().any(|d| at.starts_with(d)))
        });
        Some(Scanned {
            change: Change::new(self.path.to_string_lossy(), kind)
                .with_conflict(self.original != self.now || lands_on_another),
            path: self.path,
            original: self.original,
            now: self.now,
            mine: self.mine,
        })
    }
}

/// The changes among `found`, in its order.
pub(super) fn changes(found: Vec<Found>) -> Vec<Scanned> {
    let freed: Vec<PathBuf> = found
        .iter()
        .filter(|f| f.frees())
        .map(|f| f.path.clone())
        .collect();
    found.into_iter().filter_map(|f| f.change(&freed)).collect()
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
