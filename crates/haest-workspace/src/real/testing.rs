//! What the real disk's tests share: a FIFO, and letting go of whoever
//! waits on one.

use std::{
    ffi::CString,
    fs::OpenOptions,
    os::unix::{ffi::OsStrExt, fs::OpenOptionsExt},
    path::Path,
};

/// A FIFO (named pipe) at `path`, 0600.
#[expect(
    unsafe_code,
    reason = "std cannot make a FIFO, and these tests read and write one"
)]
pub(super) fn mkfifo(path: &Path) {
    let c = CString::new(path.as_os_str().as_bytes()).unwrap();
    // SAFETY: `c` is a NUL-terminated path that outlives the call.
    let made = unsafe { libc::mkfifo(c.as_ptr(), 0o600) };
    assert_eq!(made, 0, "mkfifo {}", path.display());
}

/// Lets a reader or a writer stuck in open() on `fifo` go: a writer finds
/// a reader that leaves at once, a reader an empty pipe.
pub(super) fn release(fifo: &Path) {
    let end = |read| {
        OpenOptions::new()
            .read(read)
            .write(!read)
            .custom_flags(libc::O_NONBLOCK)
            .open(fifo)
    };
    let reader = end(true);
    let _ = end(false);
    drop(reader);
}
