//! The real disk as a kern [`Fs`]. Rooted at `/`, so a path means what it says.
//!
//! `stat` follows symlinks (what Read and Write care about is the file a link
//! names); `read_dir` reports a link as [`FileKind::Symlink`] without following
//! it, so a walk never descends a link loop.

use std::{io, path::Path};

use async_trait::async_trait;
use kern_core::fs::{DirEntry, FileKind, Fs, FsStat};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// The host's disk through tokio's file system calls.
#[derive(Debug, Default, Clone, Copy)]
pub struct RealFs;

impl RealFs {
    pub fn new() -> Self {
        Self
    }
}

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
    #[cfg(unix)]
    let what = {
        use std::os::unix::fs::FileTypeExt;
        match () {
            _ if t.is_fifo() => "a FIFO (named pipe)",
            _ if t.is_socket() => "a socket",
            _ if t.is_char_device() || t.is_block_device() => "a device",
            _ => "not a regular file",
        }
    };
    #[cfg(not(unix))]
    let what = "not a regular file";
    Err(io::Error::new(
        io::ErrorKind::InvalidInput,
        format!("{} is {what}: only files are read", path.display()),
    ))
}

/// `path` opened to read, if it is a file. A FIFO would block in open() until
/// a writer came (out of reach of a tool's cancel), a device such as
/// /dev/zero never ends: both are refused at once. The type is checked before
/// the open, and again on the open file (opened non-blocking, so even a FIFO
/// swapped in meanwhile cannot hold it).
async fn open_file(path: &Path) -> io::Result<tokio::fs::File> {
    readable(path, tokio::fs::metadata(path).await?.file_type())?;
    let mut options = tokio::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    options.custom_flags(libc::O_NONBLOCK);
    let file = options.open(path).await?;
    readable(path, file.metadata().await?.file_type())?;
    Ok(file)
}

#[async_trait]
impl Fs for RealFs {
    async fn read(&self, path: &Path) -> io::Result<Vec<u8>> {
        let mut file = open_file(path).await?;
        let mut buf = Vec::new();
        file.read_to_end(&mut buf).await?;
        Ok(buf)
    }

    async fn read_head(&self, path: &Path, max: usize) -> io::Result<Vec<u8>> {
        let mut buf = Vec::new();
        let file = open_file(path).await?;
        file.take(max as u64).read_to_end(&mut buf).await?;
        Ok(buf)
    }

    async fn write(&self, path: &Path, data: &[u8]) -> io::Result<()> {
        tokio::fs::write(path, data).await
    }

    async fn append(&self, path: &Path, data: &[u8]) -> io::Result<()> {
        let mut f = tokio::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .await?;
        f.write_all(data).await?;
        f.flush().await
    }

    async fn exists(&self, path: &Path) -> bool {
        tokio::fs::try_exists(path).await.unwrap_or(false)
    }

    async fn stat(&self, path: &Path) -> io::Result<FsStat> {
        let m = tokio::fs::metadata(path).await?;
        Ok(FsStat {
            kind: kind_of(m.file_type()),
            size: m.len(),
            modified: m.modified().ok(),
        })
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
            out.push(DirEntry {
                name: e.file_name().to_string_lossy().into_owned(),
                kind,
            });
        }
        out.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(out)
    }

    async fn mkdir(&self, path: &Path, recursive: bool) -> io::Result<()> {
        if recursive {
            tokio::fs::create_dir_all(path).await
        } else {
            tokio::fs::create_dir(path).await
        }
    }

    async fn remove(&self, path: &Path, recursive: bool) -> io::Result<()> {
        let m = tokio::fs::symlink_metadata(path).await?;
        if !m.is_dir() {
            return tokio::fs::remove_file(path).await;
        }
        if recursive {
            tokio::fs::remove_dir_all(path).await
        } else {
            tokio::fs::remove_dir(path).await
        }
    }

    async fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
        tokio::fs::rename(from, to).await
    }

    /// Sets the permission bits (the session store makes its files 0600). A
    /// no-op where the platform has no Unix modes.
    async fn chmod(&self, path: &Path, mode: u32) -> io::Result<()> {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            tokio::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).await
        }
        #[cfg(not(unix))]
        {
            let _ = (path, mode);
            Ok(())
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use std::{
        ffi::CString, os::unix::ffi::OsStrExt, os::unix::fs::OpenOptionsExt, time::Duration,
    };

    use super::*;

    fn mkfifo(path: &Path) {
        let c = CString::new(path.as_os_str().as_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(c.as_ptr(), 0o600) }, 0);
    }

    /// Lets a reader stuck in open() on `fifo` go (it then reads an empty pipe).
    fn release(fifo: &Path) {
        let _ = std::fs::OpenOptions::new()
            .write(true)
            .custom_flags(libc::O_NONBLOCK)
            .open(fifo);
    }

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
