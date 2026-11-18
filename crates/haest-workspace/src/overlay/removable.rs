//! Whether a directory on the real disk can be deleted whole. A delete that
//! stops partway has already taken what it reached; where the writes of a
//! rename by case wait on that delete, those files would then be only in the
//! sandbox (`apply.rs`).

use std::{
    ffi::CString,
    io,
    os::unix::{ffi::OsStrExt, fs::MetadataExt},
    path::{Path, PathBuf},
};

/// The sticky bit: in a directory that has it, only an entry's owner, the
/// directory's owner or root may remove the entry.
const STICKY: u32 = 0o1000;

/// The first directory that keeps `dir` (a real path) from being deleted
/// whole: its parent, or one at or under it, that this process may not
/// list, enter or remove entries from, one with an entry the walk cannot
/// read, or a sticky one with an entry this process may not remove. None
/// when every one may be. A link inside is removed as a link, so it is not
/// followed.
///
/// Only permissions are asked: a file flag (`chflags uchg`) or an error in
/// the middle of the delete can still stop it partway.
pub(super) async fn blocker(dir: &Path) -> Option<PathBuf> {
    let owned = dir.to_path_buf();
    tokio::task::spawn_blocking(move || first_blocker(&owned))
        .await
        // The walk did not finish: nothing says the delete would.
        .unwrap_or_else(|_| Some(dir.to_path_buf()))
}

fn first_blocker(dir: &Path) -> Option<PathBuf> {
    let me = euid();
    // Removing `dir` itself is an entry its parent drops.
    if let Some(parent) = dir.parent()
        && (!may(parent, libc::W_OK | libc::X_OK) || (sticky_for(parent, me) && !owned_by(dir, me)))
    {
        return Some(parent.to_path_buf());
    }
    let mut pending = vec![dir.to_path_buf()];
    while let Some(d) = pending.pop() {
        if !may(&d, libc::R_OK | libc::W_OK | libc::X_OK) {
            return Some(d);
        }
        let entries = match std::fs::read_dir(&d) {
            Ok(entries) => entries,
            // Gone meanwhile: nothing there to stop the delete.
            Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
            Err(_) => return Some(d),
        };
        let sticky = sticky_for(&d, me);
        for entry in entries {
            // An entry the walk cannot read may be a directory it could not
            // empty.
            let Ok(entry) = entry else {
                return Some(d);
            };
            let kind = match entry.file_type() {
                Ok(kind) => kind,
                Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
                Err(_) => return Some(d),
            };
            if sticky && !owned_by(&entry.path(), me) {
                return Some(d);
            }
            if kind.is_dir() {
                pending.push(entry.path());
            }
        }
    }
    None
}

/// Whether directory `d` lets this process (effective uid `me`) remove only
/// the entries it owns. One it cannot read counts as so.
fn sticky_for(d: &Path, me: u32) -> bool {
    match std::fs::symlink_metadata(d) {
        Ok(m) => sticky_keeps(m.mode(), m.uid(), me),
        Err(_) => true,
    }
}

/// Whether a directory of `mode`, owned by `owner`, keeps a process of
/// effective uid `me` from removing an entry it does not own: it is sticky,
/// and `me` neither owns it nor is root.
fn sticky_keeps(mode: u32, owner: u32, me: u32) -> bool {
    mode & STICKY != 0 && me != 0 && owner != me
}

/// Whether `me` owns `path`, a link not followed. One it cannot read counts
/// as not.
fn owned_by(path: &Path, me: u32) -> bool {
    std::fs::symlink_metadata(path).is_ok_and(|m| m.uid() == me)
}

/// This process's effective uid.
#[expect(unsafe_code, reason = "geteuid(2) has no std wrapper")]
fn euid() -> u32 {
    // SAFETY: geteuid takes nothing, cannot fail, and reads no memory of
    // ours.
    unsafe { libc::geteuid() }
}

/// Whether this process, by its effective ids, may do `what` (`W_OK` and
/// the like) at `path`. Root may do anything the bits say nobody may.
#[expect(
    unsafe_code,
    reason = "faccessat(2) asks the kernel what this process may do; std cannot"
)]
fn may(path: &Path, what: libc::c_int) -> bool {
    let Ok(c) = CString::new(path.as_os_str().as_bytes()) else {
        return false;
    };
    // SAFETY: `c` is a NUL-terminated path that outlives the call, and
    // faccessat reads nothing else of ours.
    let rc = unsafe { libc::faccessat(libc::AT_FDCWD, c.as_ptr(), what, libc::AT_EACCESS) };
    rc == 0 || io::Error::last_os_error().kind() == io::ErrorKind::NotFound
}

#[cfg(test)]
mod tests {
    use std::{fs, os::unix::fs::PermissionsExt};

    use super::*;

    #[test]
    fn a_sticky_directory_keeps_entries_of_others_from_whoever_does_not_own_it() {
        // `/tmp`: 1777 and root's, asked by uid 501.
        assert!(sticky_keeps(0o1777, 0, 501));
        assert!(!sticky_keeps(0o1777, 501, 501), "its owner");
        assert!(!sticky_keeps(0o1777, 0, 0), "root");
        assert!(!sticky_keeps(0o777, 0, 501), "not sticky");
    }

    /// A sticky directory this process owns, holding its own files, can go.
    #[test]
    fn a_sticky_directory_of_its_own_is_no_blocker() {
        let t = tempfile::tempdir().unwrap();
        let dir = t.path().join("shared");
        fs::create_dir_all(dir.join("sub")).unwrap();
        fs::write(dir.join("sub/a"), "a\n").unwrap();
        fs::set_permissions(dir.join("sub"), fs::Permissions::from_mode(0o1777)).unwrap();
        assert_eq!(first_blocker(&dir), None);
    }
}
