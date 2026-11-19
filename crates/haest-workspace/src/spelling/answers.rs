//! What the disk answers a lookup before any listing is read: `realpath`,
//! and whether that answer is the name each directory lists.

use std::{
    ffi::OsStr,
    io,
    os::unix::{ffi::OsStrExt, fs::MetadataExt},
    path::{Path, PathBuf},
};

/// How the disk answers a lookup's first two questions: the system's own,
/// but in the tests.
#[derive(Debug, Clone, Copy)]
pub(super) struct Answers {
    /// The path as `realpath(3)` resolves it.
    pub(super) realpath: fn(&Path) -> io::Result<PathBuf>,
    /// Whether `dir` answers `realpath` with the name it lists for `name`,
    /// a name there.
    pub(super) keeps_name: fn(&Path, &OsStr) -> bool,
}

impl Answers {
    pub(super) const SYSTEM: Self = Self {
        realpath: |p| std::fs::canonicalize(p),
        keeps_name,
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

/// `rel` in the project `dir` (as mounted), whose real path is `real`, as
/// the disk first answers it. It blocks: run it off the runtime's workers.
pub(super) fn resolve(answers: Answers, dir: &Path, real: &Path, rel: &Path) -> Resolved {
    let asked = dir.join(rel);
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
    if found != real.join(rel) {
        return Resolved::Unsure;
    }
    let mut at = real.to_path_buf();
    for name in rel {
        if !(answers.keeps_name)(&at, name) {
            return Resolved::Unsure;
        }
        at.push(name);
    }
    Resolved::Listed
}

/// Whether `dir` answers `realpath` with the name it lists for `name`: on
/// a disk that answers with the name it holds (APFS, HFS+), or in a
/// directory that tells cases apart, where `name` is found only as it is
/// listed. Not one that reads `README.md` as `readme.md` and answers with
/// the spelling asked, or first looked up (exFAT, FAT, vfat, a case-folding
/// directory on Linux), nor where that cannot be told (`name` has no ASCII
/// letter).
fn keeps_name(dir: &Path, name: &OsStr) -> bool {
    answers_with_the_stored_name(dir) || tells_case_apart(dir, name)
}

/// Whether `name`, its ASCII letters' case swapped, finds nothing in `dir`,
/// or another file.
fn tells_case_apart(dir: &Path, name: &OsStr) -> bool {
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
    if swapped == name.as_bytes() {
        return false;
    }
    match std::fs::symlink_metadata(dir.join(OsStr::from_bytes(&swapped))) {
        Err(e) => e.kind() == io::ErrorKind::NotFound,
        Ok(other) => std::fs::symlink_metadata(dir.join(name))
            .is_ok_and(|own| (own.dev(), own.ino()) != (other.dev(), other.ino())),
    }
}

/// Whether `dir` is on a disk whose `realpath` answers with the name a
/// directory holds, in its case and Unicode form: APFS (either kind) and
/// HFS+. exFAT and FAT answer with the spelling the file was first looked
/// up by since the disk cached its name.
#[cfg(target_os = "macos")]
#[expect(unsafe_code, reason = "statfs(2) has no std wrapper")]
fn answers_with_the_stored_name(dir: &Path) -> bool {
    let Ok(c) = std::ffi::CString::new(dir.as_os_str().as_bytes()) else {
        return false;
    };
    let mut fs = std::mem::MaybeUninit::<libc::statfs>::uninit();
    // SAFETY: `c` is a NUL-terminated path that outlives the call, and
    // statfs writes one `struct statfs` at the pointer, which has room for
    // it.
    if unsafe { libc::statfs(c.as_ptr(), fs.as_mut_ptr()) } != 0 {
        return false;
    }
    // SAFETY: statfs succeeded, so it filled the struct in.
    let fs = unsafe { fs.assume_init() };
    let kind: Vec<u8> = fs
        .f_fstypename
        .iter()
        .take_while(|c| **c != 0)
        .map(|c| *c as u8)
        .collect();
    matches!(kind.as_slice(), b"apfs" | b"hfs")
}

/// On Linux `realpath` answers with the spelling asked.
#[cfg(not(target_os = "macos"))]
fn answers_with_the_stored_name(_: &Path) -> bool {
    false
}
