//! What the disk answers a lookup before any listing is read: `realpath`,
//! and whether that answer is the name each directory lists.

use std::{
    ffi::OsStr,
    io,
    os::unix::{ffi::OsStrExt, fs::MetadataExt},
    path::{Path, PathBuf},
};

use super::{
    disks,
    folding::{Asked, Dir, Disk, Folding},
};

/// How the disk answers a lookup's first questions: the system's own, but
/// in the tests.
#[derive(Debug, Clone, Copy)]
pub(super) struct Answers {
    /// The path as `realpath(3)` resolves it.
    pub(super) realpath: fn(&Path) -> io::Result<PathBuf>,
    /// What the disk the directory `dir` is on says of the names it holds.
    pub(super) disk: fn(&Path) -> Disk,
    /// Whether the directory `dir` reads two spellings as one by its own
    /// flag, on a [`Disk::Flagged`]; None where it cannot be read.
    pub(super) flag: fn(&Path) -> Option<bool>,
}

impl Answers {
    pub(super) const SYSTEM: Self = Self {
        realpath: |p| std::fs::canonicalize(p),
        disk: disks::disk,
        flag: disks::flag,
    };
}

/// What a lookup found before any listing is read.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Resolved {
    /// Nothing is there by any spelling: not there, or a file on the way.
    Absent,
    /// It cannot be told (a directory that cannot be searched): the call
    /// that follows answers.
    Unknown,
    /// There as asked: `realpath` gave back each name as asked, and each
    /// directory answers it with the name it lists.
    Listed,
    /// The listings decide: `realpath` spelled a name another way (another
    /// spelling, a symlink on the way), a directory may answer it with the
    /// spelling asked (exFAT, FAT, vfat), or the last name does not resolve
    /// (a symlink that dangles or loops) and is still a name its directory
    /// lists, by this spelling or not.
    Unsure,
}

/// `inside` the project `dir` (as mounted), whose real path is `real`, as
/// the disk first answers it, and what `folding` knows. It blocks: run it
/// off the runtime's workers.
pub(super) fn resolve(
    answers: Answers,
    folding: &Folding,
    dir: &Path,
    real: &Path,
    inside: &Path,
) -> Resolved {
    let asked = dir.join(inside);
    let found = match (answers.realpath)(&asked) {
        Ok(found) => found,
        Err(e) => {
            return match std::fs::symlink_metadata(&asked) {
                Ok(_) => Resolved::Unsure,
                Err(_)
                    if matches!(
                        e.kind(),
                        io::ErrorKind::NotFound | io::ErrorKind::NotADirectory
                    ) =>
                {
                    Resolved::Absent
                }
                Err(_) => Resolved::Unknown,
            };
        }
    };
    if found != real.join(inside) {
        return Resolved::Unsure;
    }
    let (mut at, mut asked) = (real.to_path_buf(), Asked::default());
    for name in inside {
        if !keeps_name(answers, folding, &mut asked, &at, name) {
            return Resolved::Unsure;
        }
        at.push(name);
    }
    Resolved::Listed
}

/// Whether `dir` answers `realpath` with the name it lists for `name`, a
/// name there: on a disk that answers with the name it holds (APFS, HFS+),
/// or in a directory that tells cases apart, where `name` is found only as
/// it is listed. Not one that reads `README.md` as `readme.md` and answers
/// with the spelling asked, or first looked up (exFAT, FAT, vfat, a
/// case-folding directory on Linux), nor one where that cannot be told yet.
///
/// A directory is asked once: by its flag where its disk keeps one, else by
/// the first name asked in it that has an ASCII letter to swap the case of
/// (`2024` and `00001` have none), or by such an entry of its listing
/// (`Spelling::learn`). A flag is read again at each lookup where the
/// system gives no birth time (musl): a directory made later under the same
/// inode number may carry another, and reading it costs no listing. A disk
/// whose answer has `realpath` trusted is asked again once per lookup
/// (`asked`): the disk under a device number may be another by the next one
/// ([`Folding::disk`]).
///
/// A case swap says nothing of Unicode forms: a directory that tells case
/// apart may still read `é` and `e\u{301}` as one (ZFS with normalization,
/// a case-sensitive APFS volume shared into Linux). So on a disk nothing
/// tells of, the swap's answer covers ASCII names alone, and the listing
/// decides any other. A flag tells more: outside a case-folding directory,
/// ext4, f2fs, tmpfs, btrfs and overlayfs keep each name's bytes.
fn keeps_name(
    answers: Answers,
    folding: &Folding,
    asked: &mut Asked,
    dir: &Path,
    name: &OsStr,
) -> bool {
    let Ok(m) = std::fs::symlink_metadata(dir) else {
        return false;
    };
    let id = Dir::of(&m);
    let disk = folding.disk(id, asked, || (answers.disk)(dir));
    match disk {
        Disk::Stored => return true,
        Disk::Folds => return false,
        Disk::Unknown if !name.as_bytes().is_ascii() => return false,
        Disk::Flagged | Disk::Unknown => {}
    }
    if let Some(folds) = folding.folds(id) {
        return !folds;
    }
    let flag = if disk == Disk::Flagged {
        (answers.flag)(dir)
    } else {
        None
    };
    let told = flag.or_else(|| folds_by_case(dir, name));
    if let Some(folds) = told
        && (flag.is_none() || id.dated())
    {
        folding.decide(id, folds);
    }
    told == Some(false)
}

/// Whether `dir` reads `name`, a name there, with its ASCII letters' case
/// swapped as the same file; None where that cannot be told: `name` has no
/// ASCII letter, a lookup fails, or `name` was replaced or removed meanwhile
/// (each answer is kept for the whole directory).
pub(super) fn folds_by_case(dir: &Path, name: &OsStr) -> Option<bool> {
    if !has_letter(name) {
        return None;
    }
    let swapped: Vec<u8> = name
        .as_bytes()
        .iter()
        .map(|b| {
            if b.is_ascii_alphabetic() {
                b ^ 0x20
            } else {
                *b
            }
        })
        .collect();
    let id = |m: std::fs::Metadata| (m.dev(), m.ino());
    let own = std::fs::symlink_metadata(dir.join(name)).ok().map(id)?;
    let other = match std::fs::symlink_metadata(dir.join(OsStr::from_bytes(&swapped))) {
        Ok(other) => Some(id(other)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => None,
        Err(_) => return None,
    };
    let still = std::fs::symlink_metadata(dir.join(name)).ok().map(id)?;
    (still == own).then_some(other == Some(own))
}

/// Whether `name` has an ASCII letter to swap the case of.
pub(super) fn has_letter(name: &OsStr) -> bool {
    name.as_bytes().iter().any(u8::is_ascii_alphabetic)
}
