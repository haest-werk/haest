//! The project overlay, recording what every MUTATION touches before it lands.
//! haest's tools and the simulated shell both go through it, so both are seen.

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

use super::Book;

type Result<T> = bashkit::Result<T>;

pub(crate) struct Recorder {
    pub(crate) inner: Arc<dyn FileSystem>,
    pub(crate) book: Arc<Book>,
}

impl Recorder {
    /// Fails with `NotFound` where the sandbox shows no directory above
    /// `path`, a new entry. bashkit's overlay makes that parent when the disk
    /// holds it, though the sandbox removed it (`rm -rf dist; echo x >
    /// dist/f`): the entry lands beneath the removed parent, hidden, and the
    /// call succeeds for a file nothing reads back. Bash fails there.
    async fn parent_shown(&self, path: &Path) -> Result<()> {
        match path.parent() {
            Some(parent) if !self.inner.exists(parent).await.unwrap_or(true) => {
                Err(io::Error::new(io::ErrorKind::NotFound, "parent directory not found").into())
            }
            _ => Ok(()),
        }
    }
}

#[async_trait]
#[warn(clippy::missing_trait_methods)]
impl FileSystemExt for Recorder {
    fn usage(&self) -> FsUsage {
        self.inner.usage()
    }
    async fn mkfifo(&self, path: &Path, mode: u32) -> Result<()> {
        self.book.remember(path).await;
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
impl FileSystem for Recorder {
    async fn read_file(&self, path: &Path) -> Result<Vec<u8>> {
        self.inner.read_file(path).await
    }
    async fn write_file(&self, path: &Path, content: &[u8]) -> Result<()> {
        self.parent_shown(path).await?;
        self.book.remember(path).await;
        self.inner.write_file(path, content).await
    }
    async fn append_file(&self, path: &Path, content: &[u8]) -> Result<()> {
        self.book.remember(path).await;
        self.inner.append_file(path, content).await
    }
    async fn mkdir(&self, path: &Path, recursive: bool) -> Result<()> {
        if recursive {
            // bashkit's overlay unhides `path` alone: a parent the sandbox
            // removed (`rm -rf dist`, then `mkdir -p dist/sub`) stayed
            // removed, and hid all made under it. So each missing parent is
            // made on its own, from the top (recursive: the overlay's upper
            // layer may lack the parents the disk holds). A directory that
            // is there has all of its own.
            let mut missing = Vec::new();
            let mut at = path.parent();
            while let Some(dir) = at
                && !self.inner.exists(dir).await.unwrap_or(true)
            {
                missing.push(dir);
                at = dir.parent();
            }
            for dir in missing.into_iter().rev() {
                self.book.remember(dir).await;
                self.inner.mkdir(dir, true).await?;
            }
        }
        self.book.remember(path).await;
        self.inner.mkdir(path, recursive).await
    }
    async fn remove(&self, path: &Path, recursive: bool) -> Result<()> {
        self.book.remember(path).await;
        self.inner.remove(path, recursive).await?;
        if recursive {
            self.book.wipe(path);
        }
        Ok(())
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
        self.parent_shown(to).await?;
        self.book.remember(from).await;
        self.book.remember(to).await;
        self.inner.rename(from, to).await?;
        // A directory moves as a whole: its old place and its new one each
        // record only the directory.
        self.book.wipe(from);
        self.book.wipe(to);
        Ok(())
    }
    async fn copy(&self, from: &Path, to: &Path) -> Result<()> {
        self.parent_shown(to).await?;
        self.book.remember(to).await;
        self.inner.copy(from, to).await
    }
    async fn symlink(&self, target: &Path, link: &Path) -> Result<()> {
        self.parent_shown(link).await?;
        self.book.remember(link).await;
        self.inner.symlink(target, link).await
    }
    async fn read_link(&self, path: &Path) -> Result<PathBuf> {
        self.inner.read_link(path).await
    }
    async fn chmod(&self, path: &Path, mode: u32) -> Result<()> {
        self.book.remember(path).await;
        self.inner.chmod(path, mode).await
    }
    async fn set_modified_time(&self, path: &Path, time: SystemTime) -> Result<()> {
        self.book.remember(path).await;
        self.inner.set_modified_time(path, time).await
    }
    fn as_search_capable(&self) -> Option<&dyn SearchCapable> {
        self.inner.as_search_capable()
    }
    async fn walk(&self, root: &Path, opts: WalkOptions) -> Result<Vec<DirListing>> {
        self.inner.walk(root, opts).await
    }
}
