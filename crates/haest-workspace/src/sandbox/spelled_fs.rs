//! The project overlay's lower layer: the real project, where a path is found
//! only as the disk spells it (`spelling.rs`). On a disk that reads `Lib` as
//! `lib`, the overlay would otherwise find the real `lib` under `Lib` after
//! the sandbox renamed or deleted it: `mv lib Lib` moved `lib` INTO it.
//!
//! Only reads are checked. The overlay never writes to its lower layer (it
//! is opened read-only), so the writes pass through untouched.

use std::{
    io,
    path::{Path, PathBuf},
    sync::Arc,
    time::SystemTime,
};

use async_trait::async_trait;
use bashkit::{
    DirEntry, DirListing, FileSystem, FileSystemExt, FsLimits, FsUsage, Metadata, SearchCapable,
    VfsSnapshot, WalkOptions,
};

use crate::spelling::{Spelled, Spelling};

type Result<T> = bashkit::Result<T>;

pub(crate) struct SpelledFs {
    inner: Arc<dyn FileSystem>,
    spelling: Spelling,
}

impl SpelledFs {
    /// `inner`, the real directory `dir`, found only as the disk spells it.
    pub(crate) fn new(inner: Arc<dyn FileSystem>, dir: &Path) -> Self {
        Self {
            inner,
            spelling: Spelling::of(dir),
        }
    }

    async fn spelled(&self, path: &Path) -> bool {
        self.spelling.spelled(path).await == Spelled::AsAsked
    }

    /// Not found, as the disk says it, where the disk holds `path` only under
    /// another spelling.
    async fn check(&self, path: &Path) -> Result<()> {
        if self.spelled(path).await {
            Ok(())
        } else {
            Err(io::Error::from_raw_os_error(libc::ENOENT).into())
        }
    }
}

#[async_trait]
#[warn(clippy::missing_trait_methods)]
impl FileSystemExt for SpelledFs {
    fn usage(&self) -> FsUsage {
        self.inner.usage()
    }
    async fn mkfifo(&self, path: &Path, mode: u32) -> Result<()> {
        self.inner.mkfifo(path, mode).await
    }
    fn limits(&self) -> FsLimits {
        self.inner.limits()
    }
    fn vfs_snapshot(&self) -> Option<VfsSnapshot> {
        self.inner.vfs_snapshot()
    }
    fn vfs_restore(&self, snapshot: &VfsSnapshot) -> Result<()> {
        self.inner.vfs_restore(snapshot)
    }
    fn backend_kind(&self) -> &'static str {
        self.inner.backend_kind()
    }
}

#[async_trait]
#[warn(clippy::missing_trait_methods)]
impl FileSystem for SpelledFs {
    async fn read_file(&self, path: &Path) -> Result<Vec<u8>> {
        self.check(path).await?;
        self.inner.read_file(path).await
    }
    async fn write_file(&self, path: &Path, content: &[u8]) -> Result<()> {
        self.inner.write_file(path, content).await
    }
    async fn append_file(&self, path: &Path, content: &[u8]) -> Result<()> {
        self.inner.append_file(path, content).await
    }
    async fn mkdir(&self, path: &Path, recursive: bool) -> Result<()> {
        self.inner.mkdir(path, recursive).await
    }
    async fn remove(&self, path: &Path, recursive: bool) -> Result<()> {
        self.inner.remove(path, recursive).await
    }
    async fn stat(&self, path: &Path) -> Result<Metadata> {
        self.check(path).await?;
        self.inner.stat(path).await
    }
    async fn lstat(&self, path: &Path) -> Result<Metadata> {
        self.check(path).await?;
        self.inner.lstat(path).await
    }
    async fn read_dir(&self, path: &Path) -> Result<Vec<DirEntry>> {
        self.check(path).await?;
        self.inner.read_dir(path).await
    }
    async fn exists(&self, path: &Path) -> Result<bool> {
        if !self.spelled(path).await {
            return Ok(false);
        }
        self.inner.exists(path).await
    }
    async fn rename(&self, from: &Path, to: &Path) -> Result<()> {
        self.inner.rename(from, to).await
    }
    async fn copy(&self, from: &Path, to: &Path) -> Result<()> {
        self.inner.copy(from, to).await
    }
    async fn symlink(&self, target: &Path, link: &Path) -> Result<()> {
        self.inner.symlink(target, link).await
    }
    async fn read_link(&self, path: &Path) -> Result<PathBuf> {
        self.check(path).await?;
        self.inner.read_link(path).await
    }
    async fn chmod(&self, path: &Path, mode: u32) -> Result<()> {
        self.inner.chmod(path, mode).await
    }
    async fn set_modified_time(&self, path: &Path, time: SystemTime) -> Result<()> {
        self.inner.set_modified_time(path, time).await
    }
    /// None: a search would read the disk unchecked.
    fn as_search_capable(&self) -> Option<&dyn SearchCapable> {
        None
    }
    /// Below `root`, every name is the one its directory lists.
    async fn walk(&self, root: &Path, opts: WalkOptions) -> Result<Vec<DirListing>> {
        self.check(root).await?;
        self.inner.walk(root, opts).await
    }
}
