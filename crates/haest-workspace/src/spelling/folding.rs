//! Which directories read two spellings of a name as one: known once per
//! directory, not once per name, and kept by the directory, not its path.

use std::{
    collections::HashMap,
    fmt,
    fs::Metadata,
    os::unix::fs::MetadataExt,
    sync::{Mutex, MutexGuard},
    time::SystemTime,
};

/// What a disk says of the names its directories hold, before any of them
/// is asked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Disk {
    /// `realpath` answers with the name a directory holds, in its case and
    /// Unicode form: APFS (either kind) and HFS+.
    #[cfg_attr(
        not(target_os = "macos"),
        expect(dead_code, reason = "only macOS has such a disk")
    )]
    Stored,
    /// Every directory reads two spellings of a name as one, and `realpath`
    /// answers with the spelling asked, or first looked up since the disk
    /// cached the name: exFAT and FAT (vfat on Linux).
    Folds,
    /// A directory reads two spellings as one only where its own flag says
    /// so (`chattr +F`): ext4, f2fs, tmpfs and btrfs, and overlayfs, which
    /// gives its layers' flag.
    Flagged,
    /// Nothing tells before a name in a directory is asked (XFS, ZFS, NFS,
    /// SMB, FUSE, ...).
    Unknown,
}

/// One directory: its device and inode number, and when it was made where
/// the system says (macOS, Linux's statx), so that one made later under a
/// freed number is another. Never when it last changed: whether it folds
/// names does not change while it holds any (`chattr +F` takes only an
/// empty directory), and a directory something keeps writing to is the one
/// to know it of.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(super) struct Dir {
    dev: u64,
    ino: u64,
    made: Option<SystemTime>,
}

impl Dir {
    pub(super) fn of(m: &Metadata) -> Self {
        Self {
            dev: m.dev(),
            ino: m.ino(),
            made: m.created().ok(),
        }
    }

    /// Whether the system said when it was made (not musl's std).
    pub(super) fn dated(self) -> bool {
        self.made.is_some()
    }
}

/// Directories known to fold names or not, past that every one is
/// forgotten and asked again.
const KEPT: usize = 65_536;

/// What each disk said, and each directory known to fold names or not.
#[derive(Default)]
pub(super) struct Folding(Mutex<Known>);

#[derive(Default)]
struct Known {
    /// By device.
    disks: HashMap<u64, Disk>,
    /// True where the directory reads two spellings as one.
    dirs: HashMap<Dir, bool>,
}

impl Folding {
    /// What the disk `dir` is on says, asked of `ask` once per device.
    pub(super) fn disk(&self, dir: Dir, ask: impl FnOnce() -> Disk) -> Disk {
        if let Some(disk) = self.lock().disks.get(&dir.dev) {
            return *disk;
        }
        let disk = ask();
        self.lock().disks.insert(dir.dev, disk);
        disk
    }

    /// Whether `dir` folds names; None where that is not known yet.
    pub(super) fn folds(&self, dir: Dir) -> Option<bool> {
        self.lock().dirs.get(&dir).copied()
    }

    pub(super) fn decide(&self, dir: Dir, folds: bool) {
        let mut known = self.lock();
        if known.dirs.len() >= KEPT {
            known.dirs.clear();
        }
        known.dirs.insert(dir, folds);
    }

    /// Whether a name in `dir` may still tell if it folds names: its disk
    /// was asked and leaves that to each directory, and nothing told yet.
    pub(super) fn undecided(&self, dir: Dir) -> bool {
        let known = self.lock();
        let per_dir = matches!(
            known.disks.get(&dir.dev),
            Some(Disk::Flagged | Disk::Unknown)
        );
        per_dir && !known.dirs.contains_key(&dir)
    }

    /// Held only to look up or keep an answer, never across an await or a
    /// system call, so a poisoned one is a bug here.
    fn lock(&self) -> MutexGuard<'_, Known> {
        self.0.lock().expect("the spelling's folding lock poisoned")
    }
}

/// How many disks and directories are known.
impl fmt::Debug for Folding {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut out = f.debug_struct("Folding");
        match self.0.try_lock() {
            Ok(known) => out
                .field("disks", &known.disks.len())
                .field("dirs", &known.dirs.len()),
            Err(_) => out.field("known", &format_args!("<locked>")),
        };
        out.finish_non_exhaustive()
    }
}
