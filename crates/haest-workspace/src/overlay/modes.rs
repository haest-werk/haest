//! A file's mode, from the sandbox to the real disk. bashkit gives a file it
//! makes 0644, whatever the disk's umask would, so only a mode the sandbox
//! chose is carried: one it set (`chmod`, kept in [`SetModes`]), and the
//! execute bits of a file it adds (a script moved or copied keeps them). A
//! file it only edits keeps the disk's mode, as an edit does in bash.
//!
//! Applying takes away any permission the sandbox took, but grants none but
//! execute, and that only to whom may read the file: `chmod +x` on a new file
//! gives what it gives under the disk's own umask, and no mode the sandbox
//! shows lets more people read or change a file than the disk does.

use std::{
    collections::BTreeMap,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    sync::{Mutex, MutexGuard},
};

use haest_core::fs::normalize;

use super::{OverlayChanges, diff, scan::ModeChange, state::Original};

/// The permission bits, setuid, setgid and sticky included.
const PERMISSIONS: u32 = 0o7777;

/// A mode the sandbox set at a path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct SetMode {
    pub(super) mode: u32,
    /// The disk's mode there when the sandbox first set one; None where the
    /// disk held nothing there (a new file, or one moved there). Another one
    /// there now means the human changed it since.
    pub(super) disk: Option<u32>,
}

/// The modes the sandbox set, by the path that has them now.
#[derive(Debug, Default)]
pub(super) struct SetModes(Mutex<BTreeMap<PathBuf, SetMode>>);

impl SetModes {
    /// The map. Its lock is held only to read or write it, never across an
    /// await, so a poisoned one is a bug here.
    fn map(&self) -> MutexGuard<'_, BTreeMap<PathBuf, SetMode>> {
        self.0.lock().expect("the overlay's modes lock poisoned")
    }

    pub(super) fn get(&self, path: &Path) -> Option<SetMode> {
        self.map().get(path).copied()
    }

    pub(super) fn insert(&self, path: PathBuf, set: SetMode) {
        self.map().insert(path, set);
    }

    /// `path` and everything under it are gone, or (`to`) moved there over
    /// what `to` had: their modes go too, or move along.
    pub(super) fn moved(&self, path: &Path, to: Option<&Path>) {
        let path = normalize(path);
        let to = to.map(normalize);
        if to.as_ref() == Some(&path) {
            return;
        }
        let mut map = self.map();
        if let Some(to) = &to {
            map.retain(|p, _| !p.starts_with(to));
        }
        let under: Vec<PathBuf> = map
            .keys()
            .filter(|p| p.starts_with(&path))
            .cloned()
            .collect();
        for old in under {
            let set = map.remove(&old).expect("listed above");
            if let (Some(to), Ok(rest)) = (&to, old.strip_prefix(&path)) {
                let disk = None;
                map.insert(normalize(&to.join(rest)), SetMode { disk, ..set });
            }
        }
    }

    /// The mode set at `path` is on the disk now.
    pub(super) fn forget(&self, path: &Path) {
        self.map().remove(path);
    }
}

impl OverlayChanges {
    /// What the mode the sandbox set at `path`, if it set one, does to the
    /// disk's. `mine` is what the overlay holds there: nothing,
    /// or a link, takes no mode.
    pub(super) async fn mode_change(&self, path: &Path, mine: Original) -> ModeChange {
        let Some(set) = self.book.modes.get(path) else {
            return ModeChange::default();
        };
        if matches!(mine, Original::Absent | Original::Link(_)) {
            return ModeChange::default();
        }
        let disk = self.book.disk_mode(path).await;
        let differs = disk.is_some_and(|d| carried(d, set.mode) != d);
        ModeChange {
            differs,
            moved: differs && set.disk.is_some() && disk != set.disk,
        }
    }

    /// `Mode <path> <disk's> -> <applied>` where applying changes the mode
    /// the disk has at `path`; empty where it does not.
    pub(super) async fn mode_line(&self, path: &Path, shown: &str) -> String {
        let Some(set) = self.book.modes.get(path) else {
            return String::new();
        };
        match self.book.disk_mode(path).await {
            Some(disk) if carried(disk, set.mode) != disk => {
                diff::mode(shown, disk, carried(disk, set.mode))
            }
            _ => String::new(),
        }
    }
}

/// The disk's mode `disk`, as the sandbox set it to `mine`: every bit both
/// have, and execute where `mine` has it and the disk lets that class read.
pub(super) fn carried(disk: u32, mine: u32) -> u32 {
    let (disk, mine) = (disk & PERMISSIONS, mine & PERMISSIONS);
    (disk & mine) | (mine & readable_execute(disk))
}

/// The disk's mode `disk` of a file the sandbox added, where it shows the
/// file as `mine`: its execute bits added, where the disk lets that class
/// read.
pub(super) fn added(disk: u32, mine: u32) -> u32 {
    (disk & PERMISSIONS) | (mine & readable_execute(disk))
}

/// The execute bits of each class `mode` lets read.
fn readable_execute(mode: u32) -> u32 {
    (mode & 0o444) >> 2
}

/// The mode of the real path `real`, a link not followed; None when nothing
/// is there.
pub(super) async fn on_disk(real: &Path) -> Option<u32> {
    let meta = tokio::fs::symlink_metadata(real).await.ok()?;
    Some(meta.permissions().mode() & PERMISSIONS)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn execute_is_granted_where_the_disk_lets_read_and_the_rest_only_taken() {
        // chmod +x under umask 022 and 077.
        assert_eq!(carried(0o644, 0o755), 0o755);
        assert_eq!(carried(0o600, 0o755), 0o700);
        // chmod -x, chmod 600.
        assert_eq!(carried(0o755, 0o644), 0o644);
        assert_eq!(carried(0o644, 0o600), 0o600);
        // chmod 666 and 4755 grant nothing.
        assert_eq!(carried(0o644, 0o666), 0o644);
        assert_eq!(carried(0o755, 0o4755), 0o755);
    }

    #[test]
    fn an_added_file_gains_execute_and_loses_nothing() {
        assert_eq!(added(0o644, 0o755), 0o755);
        assert_eq!(added(0o664, 0o644), 0o664);
        assert_eq!(added(0o600, 0o755), 0o700);
    }

    #[test]
    fn a_set_mode_moves_with_its_path_and_goes_with_it() {
        let modes = SetModes::default();
        let at = |p: &str, mode| {
            let disk = Some(0o644);
            modes.insert(PathBuf::from(p), SetMode { mode, disk });
        };
        at("/bin/run.sh", 0o755);
        at("/old.sh", 0o700);
        at("/bin2/x", 0o600);
        modes.moved(Path::new("/bin"), Some(Path::new("/bin2")));
        let moved = SetMode {
            mode: 0o755,
            disk: None,
        };
        assert_eq!(modes.get(Path::new("/bin2/run.sh")), Some(moved));
        assert_eq!(modes.get(Path::new("/bin2/x")), None, "bin2's own went");
        assert_eq!(modes.get(Path::new("/bin/run.sh")), None);
        modes.moved(Path::new("/old.sh"), None);
        assert_eq!(modes.get(Path::new("/old.sh")), None);
    }
}
