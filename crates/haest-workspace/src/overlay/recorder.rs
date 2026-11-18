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
    DirEntry, DirListing, FileSystem, FileSystemExt, FileType, FsLimits, FsUsage, Metadata,
    SearchCapable, VfsSnapshot, WalkOptions,
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

    /// The mode of the file at `path` as the sandbox shows it, None for
    /// anything else. bashkit's overlay gives a file it writes 0644, where
    /// bash keeps the file's own (`>`, `>>`, an edit): the write puts it
    /// back with [`keep_mode`](Self::keep_mode).
    async fn file_mode(&self, path: &Path) -> Option<u32> {
        let meta = self.inner.stat(path).await.ok()?;
        (meta.file_type == FileType::File).then_some(meta.mode)
    }

    /// Put `mode`, the file's own before a write, back on it. The write has
    /// landed: a mode that cannot go back changes none of its content, so
    /// that failure is not the write's.
    async fn keep_mode(&self, path: &Path, mode: Option<u32>) {
        if let Some(mode) = mode {
            let _ = self.inner.chmod(path, mode).await;
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
        let mode = self.file_mode(path).await;
        self.inner.write_file(path, content).await?;
        self.keep_mode(path, mode).await;
        Ok(())
    }
    async fn append_file(&self, path: &Path, content: &[u8]) -> Result<()> {
        // bashkit's overlay makes a file on append only where its upper
        // layer holds the directory, and never where the sandbox removed
        // one: `>>` onto a new file in a directory only the disk holds, or
        // onto one `rm` took, failed. Appending to nothing is writing, and a
        // write checks that the sandbox shows the parent, as bash does.
        if !self.inner.exists(path).await.unwrap_or(true) {
            return self.write_file(path, content).await;
        }
        self.book.remember(path).await;
        let mode = self.file_mode(path).await;
        self.inner.append_file(path, content).await?;
        self.keep_mode(path, mode).await;
        Ok(())
    }
    async fn mkdir(&self, path: &Path, recursive: bool) -> Result<()> {
        // bashkit's overlay unhides `path` alone: a parent the sandbox
        // removed (`rm -rf dist`, then `mkdir -p dist/sub`) stayed removed,
        // and hid all made under it. So each missing parent is made on its
        // own, from the top (recursive: the overlay's upper layer may lack
        // the parents the disk holds). A directory that is there has all of
        // its own.
        //
        // Not recorded, as when the overlay makes them: one the disk holds
        // is hidden only by a removal recorded at it or above it, from which
        // the scan compares what is under it, and a new one is made by
        // applying `path`. Each one recorded would be walked at every scan,
        // and the overlay lists a directory by going through all it holds.
        let mut missing = Vec::new();
        let mut at = path.parent();
        while recursive
            && let Some(dir) = at
            && !self.inner.exists(dir).await.unwrap_or(true)
        {
            missing.push(dir);
            at = dir.parent();
        }
        // The nearest one the sandbox shows is a file: bash fails ("Not a
        // directory"), where the overlay made a directory over the file.
        if let Some(dir) = at
            && self
                .inner
                .stat(dir)
                .await
                .is_ok_and(|m| !m.file_type.is_dir())
        {
            return Err(io::Error::from(io::ErrorKind::NotADirectory).into());
        }
        for dir in missing.into_iter().rev() {
            self.inner.mkdir(dir, true).await?;
        }
        self.book.remember(path).await;
        self.inner.mkdir(path, recursive).await
    }
    async fn remove(&self, path: &Path, recursive: bool) -> Result<()> {
        self.book.remember(path).await;
        self.inner.remove(path, recursive).await?;
        self.book.modes.moved(path, None);
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
        self.book.modes.moved(from, Some(to));
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
        self.inner.chmod(path, mode).await?;
        self.book.set_mode(path, mode).await;
        Ok(())
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
