//! The real disk as a haest [`Fs`]. Rooted at `/`, so a path means what it says.
//!
//! `stat` follows symlinks (what Read and Write care about is the file a link
//! names); `read_dir` reports a link as [`FileKind::Symlink`] without following
//! it, so a walk never descends a link loop.

use std::{
    io,
    path::{Path, PathBuf},
};

use async_trait::async_trait;
use haest_core::fs::{DirEntry, FileKind, Fs, FsLock, FsStat};
use tokio::io::AsyncReadExt;

/// The host's disk as a haest [`Fs`], through tokio's file system calls.
///
/// Paths mean what they say: it is rooted at `/`, so a host passes absolute
/// paths ([`resolve_path`](haest_core::fs::resolve_path) makes one).
///
/// - [`stat`](Fs::stat) follows a symlink; [`read_dir`](Fs::read_dir) reports
///   one as [`FileKind::Symlink`] without following it, so a walk never
///   descends a link loop.
/// - [`read`](Fs::read) and [`read_head`](Fs::read_head) open regular files
///   only. A FIFO, a socket or a device is refused at once with
///   [`InvalidInput`](io::ErrorKind::InvalidInput), never waited on: a FIFO
///   would block until a writer came, `/dev/zero` never ends.
/// - [`write`](Fs::write) and [`append`](Fs::append) report a write the
///   file system refused, also one an NFS client reports only as the file
///   closes (no room on the server, a quota).
/// - [`write_whole`](Fs::write_whole) puts the bytes in a new file beside
///   the file, its owner's alone until they are all in, gives it the
///   file's permission bits, owner and group (a run as root keeps a user's
///   file theirs) and renames it over the file, so a write the disk stops
///   partway (a full disk, a kill) leaves the file as it was. The overlay's
///   apply and the tools that rewrite a file go through it. On a disk that
///   reads two spellings of a name as one, the system decides which one
///   the file goes by afterwards, as for any program that saves by
///   renaming. A file made read-only is refused. It writes in place, as
///   `write` does, where a new file would not stand in for the file
///   (another hard link to it, a symlink, a FIFO or a device, which it
///   opens once, as `write` does) or its directory takes no new name: there
///   a full disk still cuts the file short, and says so. A file whose
///   owner, group or mode the new one cannot take (another user's, a group
///   the run is not in, a share that sets no modes), or that cannot be
///   renamed over (one file mounted on its own), fails the write and stays
///   as it was. A replaced file does not keep its extended attributes or
///   ACLs, and whatever holds the old file keeps it: a process that has it
///   open (a log a server appends to, `tail -f`), an inotify watch on it, a
///   container it is bind-mounted into on its own. The disk needs room for
///   the second copy, and a kill partway can leave a `.haest-*` file
///   beside it, no more open than the file.
/// - [`chmod`](Fs::chmod) sets Unix permission bits (a no-op elsewhere).
/// - [`try_lock`](Fs::try_lock) takes an exclusive `flock(2)`, which the
///   system drops with the process that held it. It opens the file to read
///   and write for it (an NFS client refuses an exclusive lock on a file
///   opened only to read), or only to read where the file may not be
///   written (made read-only, or on a read-only mount). A file that is not
///   there is made, empty and 0600.
///
/// # Examples
///
/// ```
/// use haest_core::fs::Fs;
/// use haest_workspace::RealFs;
///
/// # #[tokio::main]
/// # async fn main() -> std::io::Result<()> {
/// let dir = tempfile::tempdir()?;
/// let path = dir.path().join("a.txt");
/// RealFs.write(&path, b"hello").await?;
/// assert_eq!(RealFs.read_head(&path, 2).await?, b"he");
/// assert!(RealFs.read(std::path::Path::new("/dev/null")).await.is_err());
/// # Ok(())
/// # }
/// ```
#[derive(Debug, Default, Clone, Copy)]
pub struct RealFs;

fn kind_of(t: std::fs::FileType) -> FileKind {
    if t.is_symlink() {
        FileKind::Symlink
    } else if t.is_dir() {
        FileKind::Dir
    } else {
        FileKind::File
    }
}

/// Err unless `t` is a file or a directory (which fails on read, as it did).
fn readable(path: &Path, t: std::fs::FileType) -> io::Result<()> {
    if t.is_file() || t.is_dir() {
        return Ok(());
    }
    use std::os::unix::fs::FileTypeExt;
    let what = match () {
        _ if t.is_fifo() => "a FIFO (named pipe)",
        _ if t.is_socket() => "a socket",
        _ if t.is_char_device() || t.is_block_device() => "a device",
        _ => "not a regular file",
    };
    Err(io::Error::new(
        io::ErrorKind::InvalidInput,
        format!("{} is {what}: only files are read", path.display()),
    ))
}

/// `path` opened to read (and to write as well, when `write`), if it is a
/// file. A FIFO would block in open() until a writer came (out of reach of a
/// tool's cancel), a device such as /dev/zero never ends: both are refused
/// at once. The type is checked before the open, and again on the open file
/// (opened non-blocking, so even a FIFO swapped in meanwhile cannot hold it).
pub(super) async fn open_file(path: &Path, write: bool) -> io::Result<tokio::fs::File> {
    readable(path, tokio::fs::metadata(path).await?.file_type())?;
    let mut options = tokio::fs::OpenOptions::new();
    options
        .read(true)
        .write(write)
        .custom_flags(libc::O_NONBLOCK);
    let file = options.open(path).await?;
    readable(path, file.metadata().await?.file_type())?;
    Ok(file)
}

#[async_trait]
impl Fs for RealFs {
    async fn read(&self, path: &Path) -> io::Result<Vec<u8>> {
        let mut file = open_file(path, false).await?;
        let mut buf = Vec::new();
        file.read_to_end(&mut buf).await?;
        Ok(buf)
    }

    async fn read_head(&self, path: &Path, max: usize) -> io::Result<Vec<u8>> {
        let mut buf = Vec::new();
        let file = open_file(path, false).await?;
        file.take(max as u64).read_to_end(&mut buf).await?;
        Ok(buf)
    }

    /// Makes the file, or empties it first and keeps its mode, and writes
    /// all of `data`. A write the file system refused is an error, on a
    /// network file system too, where an NFS client reports it only as the
    /// file closes (no room on the server, a quota).
    async fn write(&self, path: &Path, data: &[u8]) -> io::Result<()> {
        super::write::write(path, data).await
    }

    /// Makes the file if it is missing, and adds all of `data` to its end.
    /// A write the file system refused is an error, on a network file
    /// system too, where an NFS client reports it only as the file closes:
    /// a session record that did not land is never read as written.
    async fn append(&self, path: &Path, data: &[u8]) -> io::Result<()> {
        super::write::append(path, data).await
    }

    /// The bytes go into a new file beside it, which is renamed over it,
    /// so a write the disk stops partway leaves it as it was; where a new
    /// file would not stand in for it, they go in place (`replace.rs`).
    async fn write_whole(&self, path: &Path, data: &[u8]) -> io::Result<()> {
        super::replace::put_file(path, data).await
    }

    async fn exists(&self, path: &Path) -> bool {
        tokio::fs::try_exists(path).await.unwrap_or(false)
    }

    async fn stat(&self, path: &Path) -> io::Result<FsStat> {
        let m = tokio::fs::metadata(path).await?;
        let stat = FsStat::new(kind_of(m.file_type()), m.len());
        // A platform that keeps no time says so with an error: no time, then.
        Ok(match m.modified() {
            Ok(at) => stat.with_modified(at),
            Err(_) => stat,
        })
    }

    async fn read_link(&self, path: &Path) -> io::Result<PathBuf> {
        tokio::fs::read_link(path).await
    }

    async fn read_dir(&self, path: &Path) -> io::Result<Vec<DirEntry>> {
        let mut rd = tokio::fs::read_dir(path).await?;
        let mut out = Vec::new();
        while let Some(e) = rd.next_entry().await? {
            let kind = match e.file_type().await {
                Ok(t) => kind_of(t),
                // Vanished between the listing and the look: skip it.
                Err(_) => continue,
            };
            out.push(DirEntry::new(e.file_name().to_string_lossy(), kind));
        }
        out.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(out)
    }

    async fn create_dir(&self, path: &Path) -> io::Result<()> {
        tokio::fs::create_dir(path).await
    }

    async fn create_dir_all(&self, path: &Path) -> io::Result<()> {
        tokio::fs::create_dir_all(path).await
    }

    async fn remove(&self, path: &Path) -> io::Result<()> {
        let m = tokio::fs::symlink_metadata(path).await?;
        if m.is_dir() {
            tokio::fs::remove_dir(path).await
        } else {
            tokio::fs::remove_file(path).await
        }
    }

    async fn remove_all(&self, path: &Path) -> io::Result<()> {
        let m = tokio::fs::symlink_metadata(path).await?;
        if m.is_dir() {
            tokio::fs::remove_dir_all(path).await
        } else {
            tokio::fs::remove_file(path).await
        }
    }

    async fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
        tokio::fs::rename(from, to).await
    }

    /// Sets the permission bits (the session store makes its files 0600).
    async fn chmod(&self, path: &Path, mode: u32) -> io::Result<()> {
        use std::os::unix::fs::PermissionsExt;
        tokio::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).await
    }

    /// An exclusive `flock(2)` on the file, opened to read and write: an NFS
    /// client takes the lock as a write lock on the server, and refuses it
    /// on a file opened only to read. Where the file or its mount may not be
    /// written (`PermissionDenied`, `ReadOnlyFilesystem`), it is opened only
    /// to read, which a local disk locks all the same. A file that is not
    /// there is made, empty and 0600 (its dir must be). Dropping the lock
    /// closes the file. The system drops it with the process, so a crash
    /// never leaves the file locked. A child this process forks shares it
    /// until the child starts its program, which closes the file (it is
    /// opened close-on-exec). `Ok(None)` where the file's file system takes
    /// no such locks, or refuses them on this file.
    async fn try_lock(&self, path: &Path) -> io::Result<Option<FsLock>> {
        super::lock::exclusive(path).await
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::real::testing::{mkfifo, release};

    #[tokio::test]
    async fn a_fifo_is_refused_at_once_never_waited_on() {
        let dir = tempfile::tempdir().unwrap();
        let fifo = dir.path().join("pipe");
        mkfifo(&fifo);
        let wait = Duration::from_secs(2);
        let read = tokio::time::timeout(wait, RealFs.read(&fifo)).await;
        release(&fifo);
        let head = tokio::time::timeout(wait, RealFs.read_head(&fifo, 10)).await;
        release(&fifo);
        assert!(matches!(read, Ok(Err(_))), "read: {read:?}");
        assert!(matches!(head, Ok(Err(_))), "read_head: {head:?}");
    }

    #[tokio::test]
    async fn a_device_or_a_socket_is_refused_and_a_file_still_reads() {
        // /dev/null ends at once either way; /dev/zero never would.
        assert!(RealFs.read(Path::new("/dev/null")).await.is_err());
        let dir = tempfile::tempdir().unwrap();
        let sock = dir.path().join("sock");
        let _listener = std::os::unix::net::UnixListener::bind(&sock).unwrap();
        assert!(RealFs.read(&sock).await.is_err());
        let file = dir.path().join("a.txt");
        std::fs::write(&file, "hello").unwrap();
        assert_eq!(RealFs.read(&file).await.unwrap(), b"hello");
        assert_eq!(RealFs.read_head(&file, 2).await.unwrap(), b"he");
        // A link to a device is the device.
        std::os::unix::fs::symlink("/dev/null", dir.path().join("null")).unwrap();
        assert!(RealFs.read(&dir.path().join("null")).await.is_err());
    }
}
