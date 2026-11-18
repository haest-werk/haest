//! A file's mode, from the sandbox to the real disk. bashkit gives a file it
//! makes 0644, whatever the disk's umask would, so only a mode the sandbox
//! chose is carried: one it set (`chmod`), one a path took along when the
//! sandbox moved or copied it (`mv` keeps a path's mode, and `cp` gives a
//! new file its source's), both kept in [`SetModes`], and the execute bits
//! of a file it adds. A file it only edits keeps the disk's mode, as an
//! edit does in bash.
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

use haest_core::fs::{Fs, normalize};

use super::{
    OverlayChanges, diff,
    scan::{ModeChange, under},
    state::Original,
};

/// The permission bits, setuid, setgid and sticky included.
pub(super) const PERMISSIONS: u32 = 0o7777;

/// A mode the sandbox set at a path, or a move or a copy brought there.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct SetMode {
    pub(super) mode: u32,
    /// The disk's mode there when the sandbox first set or brought one; None
    /// where the disk held nothing there (a new file). Another one there now
    /// means the human changed it since.
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

    /// `path` and everything under it are gone (removed, or moved away):
    /// their modes go too.
    pub(super) fn gone(&self, path: &Path) {
        let path = normalize(path);
        let mut map = self.map();
        let below: Vec<PathBuf> = under(&map, &path).map(|(p, _)| p.clone()).collect();
        for p in below {
            map.remove(&p);
        }
        map.remove(&path);
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
    /// the disk has at `path`, and `Mode <path> <applied>` for a path the
    /// disk does not hold, where its mode is its own (git's "new file
    /// mode"); empty for any other.
    pub(super) async fn mode_line(&self, path: &Path, shown: &str) -> String {
        let Some(set) = self.book.modes.get(path) else {
            return String::new();
        };
        if let Some(disk) = self.book.disk_mode(path).await {
            let applied = carried(disk, set.mode);
            return if applied == disk {
                String::new()
            } else {
                diff::mode(shown, Some(disk), applied)
            };
        }
        // Added: the disk makes it, at the usual umask's mode or narrower,
        // and the sandbox's mode goes on that.
        let made = match self.overlay.stat(path).await {
            Ok(st) if st.is_dir() => MADE_DIR,
            Ok(_) => MADE_FILE,
            Err(_) => return String::new(),
        };
        let applied = carried(made, set.mode);
        if applied == made {
            String::new()
        } else {
            diff::mode(shown, None, applied)
        }
    }
}

/// The mode bashkit gives a file it makes, and a directory: what bash
/// gives them under the usual umask (022).
const MADE_FILE: u32 = 0o644;
const MADE_DIR: u32 = 0o755;

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
        // A 0644 file moved in, made 0664 under umask 002, and a 0600 key.
        assert_eq!(carried(0o664, 0o644), 0o644);
        assert_eq!(carried(0o644, 0o600), 0o600);
    }

    #[test]
    fn an_added_file_gains_execute_and_loses_nothing() {
        assert_eq!(added(0o644, 0o755), 0o755);
        assert_eq!(added(0o664, 0o644), 0o664);
        assert_eq!(added(0o600, 0o755), 0o700);
    }

    #[test]
    fn a_set_mode_goes_with_its_path_and_only_with_it() {
        let modes = SetModes::default();
        let all = [
            "/bin",
            "/bin/run.sh",
            "/bin/sub/x",
            "/bin2/x",
            "/bin.d",
            "/b",
        ];
        for p in all {
            let set = SetMode {
                mode: 0o755,
                disk: Some(0o644),
            };
            modes.insert(PathBuf::from(p), set);
        }
        modes.gone(Path::new("/bin/"));
        let left: Vec<_> = all
            .into_iter()
            .filter(|p| modes.get(Path::new(p)).is_some())
            .collect();
        assert_eq!(left, ["/bin2/x", "/bin.d", "/b"]);
    }
}
