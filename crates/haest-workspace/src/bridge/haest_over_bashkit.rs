//! A bashkit file system as a haest [`Fs`]: what the tools see in the sandbox.
//!
//! bashkit's in-memory layers never follow a symlink themselves (the shell's
//! outermost layer does). This view follows one in the LAST path component for
//! the operations that open a file -- `read`, `write`, `append`, `stat`, `chmod` -- the
//! way the shell's `cat` does, and lists a link as [`FileKind::Symlink`]
//! without following it. A link's target is a path in the same namespace (the
//! sandbox mounts every directory at its real path, so an absolute target
//! still means what it says).

use std::{
    io,
    path::{Path, PathBuf},
    sync::Arc,
};

use async_trait::async_trait;
use bashkit::{FileSystem, FileType, Metadata};
use haest_core::fs::{DirEntry, FileKind, Fs, FsStat, normalize};

use super::io_from_bashkit;

/// Links followed per lookup before giving up (Linux MAXSYMLINKS).
const MAX_HOPS: usize = 40;

/// A bashkit `FileSystem` as a haest [`Fs`]: how haest's tools see the files of
/// the default sandbox, the same files its shell sees.
///
/// bashkit's in-memory layers never follow a symlink themselves. This view
/// follows one in the LAST path component for `read`, `write`, `append`,
/// `stat` and `read_dir` (up to 40 links, then an error), the way the shell's
/// `cat` does, and lists a link as [`FileKind::Symlink`] without following
/// it. [`chmod`](Fs::chmod) follows a link too, and sets the bits wherever the
/// path lives: on the real disk through a writable mount, in memory elsewhere.
pub struct HaestFsOverBashkit {
    inner: Arc<dyn FileSystem>,
}

impl HaestFsOverBashkit {
    /// Wrap `inner` so haest's tools can read and write it.
    pub fn new(inner: Arc<dyn FileSystem>) -> Self {
        Self { inner }
    }

    /// The bashkit file system this view reads and writes.
    pub fn inner(&self) -> &Arc<dyn FileSystem> {
        &self.inner
    }

    /// `path` with a symlink in its last component replaced by what it names.
    /// A path that does not exist resolves to itself (the caller's op reports it).
    async fn follow(&self, path: &Path) -> io::Result<PathBuf> {
        let mut p = normalize(path);
        for _ in 0..MAX_HOPS {
            match self.inner.stat(&p).await {
                Ok(m) if m.file_type.is_symlink() => {
                    let target = self.inner.read_link(&p).await.map_err(io_from_bashkit)?;
                    let base = p.parent().unwrap_or(Path::new("/"));
                    p = normalize(&base.join(target));
                }
                _ => return Ok(p),
            }
        }
        Err(io::Error::other("Too many levels of symbolic links"))
    }
}

fn kind_of(t: FileType) -> FileKind {
    match t {
        FileType::Directory => FileKind::Dir,
        FileType::Symlink => FileKind::Symlink,
        FileType::File | FileType::Fifo => FileKind::File,
    }
}

fn stat_of(m: &Metadata) -> FsStat {
    FsStat {
        kind: kind_of(m.file_type),
        size: m.size,
        modified: Some(m.modified),
    }
}

#[async_trait]
impl Fs for HaestFsOverBashkit {
    async fn read(&self, path: &Path) -> io::Result<Vec<u8>> {
        // A layer over the real disk reads through its links itself: ask it first.
        let p = normalize(path);
        let err = match self.inner.read_file(&p).await {
            Ok(bytes) => return Ok(bytes),
            Err(e) => e,
        };
        let target = self.follow(&p).await?;
        if target == p {
            return Err(io_from_bashkit(err));
        }
        self.inner.read_file(&target).await.map_err(io_from_bashkit)
    }

    async fn write(&self, path: &Path, data: &[u8]) -> io::Result<()> {
        let p = self.follow(path).await?;
        self.inner
            .write_file(&p, data)
            .await
            .map_err(io_from_bashkit)
    }

    async fn append(&self, path: &Path, data: &[u8]) -> io::Result<()> {
        let p = self.follow(path).await?;
        self.inner
            .append_file(&p, data)
            .await
            .map_err(io_from_bashkit)
    }

    async fn exists(&self, path: &Path) -> bool {
        self.inner.exists(&normalize(path)).await.unwrap_or(false)
    }

    async fn stat(&self, path: &Path) -> io::Result<FsStat> {
        let p = self.follow(path).await?;
        let m = self.inner.stat(&p).await.map_err(io_from_bashkit)?;
        Ok(stat_of(&m))
    }

    async fn read_link(&self, path: &Path) -> io::Result<PathBuf> {
        self.inner
            .read_link(&normalize(path))
            .await
            .map_err(io_from_bashkit)
    }

    async fn read_dir(&self, path: &Path) -> io::Result<Vec<DirEntry>> {
        let p = self.follow(path).await?;
        let mut out: Vec<DirEntry> = self
            .inner
            .read_dir(&p)
            .await
            .map_err(io_from_bashkit)?
            .into_iter()
            .map(|e| DirEntry {
                kind: kind_of(e.metadata.file_type),
                name: e.name,
            })
            .collect();
        out.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(out)
    }

    async fn mkdir(&self, path: &Path, recursive: bool) -> io::Result<()> {
        self.inner
            .mkdir(&normalize(path), recursive)
            .await
            .map_err(io_from_bashkit)
    }

    async fn remove(&self, path: &Path, recursive: bool) -> io::Result<()> {
        self.inner
            .remove(&normalize(path), recursive)
            .await
            .map_err(io_from_bashkit)
    }

    async fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
        self.inner
            .rename(&normalize(from), &normalize(to))
            .await
            .map_err(io_from_bashkit)
    }

    async fn chmod(&self, path: &Path, mode: u32) -> io::Result<()> {
        let p = self.follow(path).await?;
        self.inner.chmod(&p, mode).await.map_err(io_from_bashkit)
    }
}
