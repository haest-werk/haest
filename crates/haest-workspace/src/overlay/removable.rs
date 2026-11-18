//! Whether a directory on the real disk can be deleted whole. A delete that
//! stops partway has already taken what it reached; where the writes of a
//! rename by case wait on that delete, those files would then be only in the
//! sandbox (`apply.rs`).

use std::{
    ffi::CString,
    io,
    os::unix::ffi::OsStrExt,
    path::{Path, PathBuf},
};

/// The first directory that keeps `dir` (a real path) from being deleted
/// whole: its parent, or one at or under it, that this process may not
/// list, enter or remove entries from. None when every one may be. A link
/// inside is removed as a link, so it is not followed.
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
    // Removing `dir` itself is an entry its parent drops.
    if let Some(parent) = dir.parent()
        && !may(parent, libc::W_OK | libc::X_OK)
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
        for entry in entries.flatten() {
            if entry.file_type().is_ok_and(|t| t.is_dir()) {
                pending.push(entry.path());
            }
        }
    }
    None
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
