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

#[async_trait]
impl Fs for RealFs {
    async fn read(&self, path: &Path) -> io::Result<Vec<u8>> {
        tokio::fs::read(path).await
    }

    async fn read_head(&self, path: &Path, max: usize) -> io::Result<Vec<u8>> {
        let mut buf = Vec::new();
        let file = tokio::fs::File::open(path).await?;
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
