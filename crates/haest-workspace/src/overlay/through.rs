//! Once a change is on the disk, the sandbox reads the disk there again.
//! bashkit's overlay keeps its own copy of each file the sandbox wrote, and a
//! mark that hides each name it deleted, for as long as it lives: after an
//! apply, the sandbox would read its old copy, or nothing, whatever the human
//! did on the disk since, and its next change would go over the human's with
//! no conflict (the ledger takes what the disk holds at the next touch as the
//! path's first state).

use std::path::{Path, PathBuf};

use bashkit::{FileSystem, OverlayFs};
use haest_core::workspace::{Change, ChangeKind};

use super::{OverlayChanges, Scanned, state::Original, state::state_of};
use crate::spelling::Ways;

impl OverlayChanges {
    /// `s` is applied, and `took` are the names the disk held under what it
    /// removed: the sandbox's copy at `s` goes, and so does the mark hiding
    /// each name deleted, so the sandbox reads the disk there. Unless the
    /// sandbox changed `s` again since the scan: that is its own.
    pub(super) async fn read_through(&self, s: &Scanned, took: &[PathBuf]) {
        let Some(cow) = &self.cow else {
            return;
        };
        if state_of(&self.overlay, &s.path).await != s.mine {
            return;
        }
        if matches!(s.mine, Original::File(_)) {
            let _ = cow.upper().remove(&s.path, false).await;
        }
        let gone = (s.mine == Original::Absent).then_some(&s.path);
        for path in took.iter().chain(gone) {
            unhide(cow, path).await;
        }
    }

    /// After all of an apply's changes: each directory on the way to one
    /// it put on the disk whose copy in the sandbox holds nothing of its own
    /// any more (each file in it read through, and `mkdir -p`'s parents,
    /// which apply makes with it) goes too, where the disk holds that
    /// directory, so the sandbox reads the disk's.
    pub(super) async fn read_through_dirs(&self, applied: &[Change]) {
        let Some(cow) = &self.cow else {
            return;
        };
        let upper = cow.upper();
        let mut ways = Ways::default();
        // In path order, reversed: a directory after those inside it.
        for c in applied
            .iter()
            .rev()
            .filter(|c| c.kind != ChangeKind::Deleted)
        {
            let changed = Path::new(&c.path);
            for dir in changed.ancestors().filter(|d| d.parent().is_some()) {
                let empty = upper.read_dir(dir).await.is_ok_and(|e| e.is_empty());
                if empty && self.book.on_disk(dir, &mut ways).await.state == Original::Dir {
                    let _ = upper.remove(dir, false).await;
                } else if dir != changed {
                    break;
                }
            }
        }
    }
}

/// Drop the mark that hides `path`, a name the apply deleted on the disk.
/// bashkit's overlay drops it only where it makes the name, so the name is
/// made in its upper layer and taken out again, with every directory making
/// it added there. A name the upper layer holds is the sandbox's own.
async fn unhide(cow: &OverlayFs, path: &Path) {
    let upper = cow.upper();
    if upper.exists(path).await.unwrap_or(true) {
        return;
    }
    // The highest directory on the way the upper layer lacks: made with the
    // name, and taken out with it.
    let mut top = path;
    while let Some(parent) = top.parent().filter(|p| p.parent().is_some())
        && !upper.exists(parent).await.unwrap_or(true)
    {
        top = parent;
    }
    if cow.mkdir(path, true).await.is_ok() {
        let _ = upper.remove(top, true).await;
    }
}
