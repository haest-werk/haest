//! A file written to its end, close(2)'s answer included. An NFS client
//! sends what was written as the file closes, and reports a write the
//! server refused (no room, a quota) only there; a dropped `File` throws
//! that answer away, so the write would read as done with the file cut
//! short.

use std::{
    fs::{File, OpenOptions},
    io::{self, Write},
    os::fd::IntoRawFd,
    path::Path,
};

/// `data` into the file at `path`, made if missing and emptied first if
/// not, as [`std::fs::write`] does, with its close checked.
pub(super) async fn write(path: &Path, data: &[u8]) -> io::Result<()> {
    let mut options = OpenOptions::new();
    options.write(true).create(true).truncate(true);
    write_closed(options, path, data).await
}

/// `data` onto the end of the file at `path`, made if missing, with its
/// close checked.
pub(super) async fn append(path: &Path, data: &[u8]) -> io::Result<()> {
    let mut options = OpenOptions::new();
    options.append(true).create(true);
    write_closed(options, path, data).await
}

/// Open `path` with `options`, write all of `data` and close it, as one
/// blocking task: the error close(2) returns is the write's.
async fn write_closed(options: OpenOptions, path: &Path, data: &[u8]) -> io::Result<()> {
    let (path, data) = (path.to_owned(), data.to_vec());
    tokio::task::spawn_blocking(move || {
        let mut file = options.open(&path)?;
        file.write_all(&data)?;
        close(file)
    })
    .await
    .map_err(io::Error::other)?
}

/// Close `file`, with close(2)'s answer, which dropping it ignores. Never
/// retried, as std never retries it: a retry could close a descriptor
/// another thread opened meanwhile. An interrupted close fails the write,
/// the safe side.
#[expect(
    unsafe_code,
    reason = "std ignores close(2)'s error, and only libc's close returns it"
)]
fn close(file: File) -> io::Result<()> {
    let fd = file.into_raw_fd();
    // SAFETY: `fd` was the file's own descriptor; `into_raw_fd` took it
    // from the file, so nothing else closes it, and it is closed once, here.
    if unsafe { libc::close(fd) } == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(test)]
mod tests {
    use std::{fs, io, os::unix::fs::PermissionsExt, path::Path};

    use haest_core::fs::Fs;

    use crate::RealFs;

    fn mode(path: &Path) -> u32 {
        fs::metadata(path).unwrap().permissions().mode() & 0o7777
    }

    fn set_mode(path: &Path, mode: u32) {
        fs::set_permissions(path, fs::Permissions::from_mode(mode)).unwrap();
    }

    /// A write makes the file, or empties it first and keeps its mode (the
    /// apply makes a new file its owner's alone, then fills it).
    #[tokio::test]
    async fn a_write_makes_the_file_or_empties_it_and_keeps_its_mode() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("a.txt");
        let first: &[u8] = b"a longer first version";
        RealFs.write(&file, first).await.unwrap();
        assert_eq!(fs::read(&file).unwrap(), first);
        set_mode(&file, 0o600);
        RealFs.write(&file, b"short").await.unwrap();
        assert_eq!(fs::read(&file).unwrap(), b"short");
        assert_eq!(mode(&file), 0o600);
        RealFs.write(&file, b"").await.unwrap();
        assert_eq!(fs::read(&file).unwrap(), b"");
        let missing = dir.path().join("missing/a.txt");
        let refused = RealFs.write(&missing, b"x").await.map_err(|e| e.kind());
        assert_eq!(refused, Err(io::ErrorKind::NotFound));
    }

    /// An append makes the file, or adds to its end and keeps its mode. One
    /// of nothing to a file made read-only is refused (the apply asks so).
    #[tokio::test]
    async fn an_append_makes_the_file_or_adds_to_its_end() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("s.jsonl");
        RealFs.append(&file, b"{\"one\":1}\n").await.unwrap();
        set_mode(&file, 0o600);
        RealFs.append(&file, b"{\"two\":2}\n").await.unwrap();
        RealFs.append(&file, b"").await.unwrap();
        assert_eq!(fs::read(&file).unwrap(), b"{\"one\":1}\n{\"two\":2}\n");
        assert_eq!(mode(&file), 0o600);
        set_mode(&file, 0o400);
        let refused = RealFs.append(&file, b"").await.map_err(|e| e.kind());
        assert_eq!(refused, Err(io::ErrorKind::PermissionDenied));
        let onto_dir = RealFs.append(dir.path(), b"x").await;
        assert!(onto_dir.is_err(), "onto a directory: {onto_dir:?}");
    }

    /// Names a directory on an NFS mount with less than 1 MiB free, for the
    /// test of a write the server refuses; unset, the test passes without
    /// running.
    const FULL_NFS_DIR: &str = "HAEST_TEST_FULL_NFS_DIR";

    /// An NFS client sends what was written as the file closes, and reports
    /// a write the server refused (no room, a quota) only there. Both read
    /// as written: a session record the server never took was lost without
    /// a word.
    #[tokio::test]
    async fn a_write_or_an_append_an_nfs_server_refuses_is_an_error() {
        let Some(base) = std::env::var_os(FULL_NFS_DIR) else {
            return;
        };
        let dir = tempfile::tempdir_in(base).unwrap();
        // One write(2) the client takes whole, as a session record is.
        let more = vec![b'x'; 1 << 20];
        let file = dir.path().join("w.txt");
        let wrote = RealFs.write(&file, &more).await;
        // What of it the server took makes room again for the append.
        let _ = fs::remove_file(&file);
        let appended = RealFs.append(&dir.path().join("a.jsonl"), &more).await;
        let refused = wrote.is_err() && appended.is_err();
        assert!(refused, "the write: {wrote:?}; the append: {appended:?}");
    }
}
