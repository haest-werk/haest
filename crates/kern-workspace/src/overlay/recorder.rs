//! The project overlay, recording what every MUTATION touches before it lands.
//! kern's tools and the simulated shell both go through it, so both are seen.

use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::SystemTime,
};

use async_trait::async_trait;
use bashkit::{
    DirEntry, DirListing, FileSystem, FileSystemExt, FsLimits, FsUsage, Metadata, SearchCapable,
    VfsSnapshot, WalkOptions,
};

use super::Book;

type Result<T> = bashkit::Result<T>;

pub(crate) struct Recorder {
    pub(crate) inner: Arc<dyn FileSystem>,
    pub(crate) book: Arc<Book>,
}

impl Recorder {
    async fn touch(&self, path: &Path) {
        self.book.remember(path).await;
    }
}

#[async_trait]
impl FileSystemExt for Recorder {
    fn usage(&self) -> FsUsage {
        self.inner.usage()
    }
    async fn mkfifo(&self, path: &Path, mode: u32) -> Result<()> {
        self.touch(path).await;
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
impl FileSystem for Recorder {
    async fn read_file(&self, path: &Path) -> Result<Vec<u8>> {
        self.inner.read_file(path).await
    }
    async fn write_file(&self, path: &Path, content: &[u8]) -> Result<()> {
        self.touch(path).await;
        self.inner.write_file(path, content).await
    }
    async fn append_file(&self, path: &Path, content: &[u8]) -> Result<()> {
        self.touch(path).await;
        self.inner.append_file(path, content).await
    }
    async fn mkdir(&self, path: &Path, recursive: bool) -> Result<()> {
        self.touch(path).await;
        self.inner.mkdir(path, recursive).await
    }
    async fn remove(&self, path: &Path, recursive: bool) -> Result<()> {
        self.touch(path).await;
        self.inner.remove(path, recursive).await
    }
    async fn stat(&self, path: &Path) -> Result<Metadata> {
        self.inner.stat(path).await
    }
    async fn lstat(&self, path: &Path) -> Result<Metadata> {
        self.inner.lstat(path).await
    }
    async fn read_dir(&self, path: &Path) -> Result<Vec<DirEntry>> {
        self.inner.read_dir(path).await
    }
    async fn exists(&self, path: &Path) -> Result<bool> {
        self.inner.exists(path).await
    }
    async fn rename(&self, from: &Path, to: &Path) -> Result<()> {
        self.touch(from).await;
        self.touch(to).await;
        self.inner.rename(from, to).await
    }
    async fn copy(&self, from: &Path, to: &Path) -> Result<()> {
        self.touch(to).await;
        self.inner.copy(from, to).await
    }
    async fn symlink(&self, target: &Path, link: &Path) -> Result<()> {
        self.touch(link).await;
        self.inner.symlink(target, link).await
    }
    async fn read_link(&self, path: &Path) -> Result<PathBuf> {
        self.inner.read_link(path).await
    }
    async fn chmod(&self, path: &Path, mode: u32) -> Result<()> {
        self.touch(path).await;
        self.inner.chmod(path, mode).await
    }
    async fn set_modified_time(&self, path: &Path, time: SystemTime) -> Result<()> {
        self.touch(path).await;
        self.inner.set_modified_time(path, time).await
    }
    fn as_search_capable(&self) -> Option<&dyn SearchCapable> {
        self.inner.as_search_capable()
    }
    async fn walk(&self, root: &Path, opts: WalkOptions) -> Result<Vec<DirListing>> {
        self.inner.walk(root, opts).await
    }
}
