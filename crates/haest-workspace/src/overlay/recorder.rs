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
use haest_core::fs::normalize;
use tokio::sync::RwLockReadGuard;

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

    /// The sandbox's turn to change the project: shared with its other
    /// changes, never with an apply. Taken once per change, never twice by
    /// one (a second would wait behind an apply that waits for the first).
    async fn turn(&self) -> RwLockReadGuard<'_, ()> {
        self.book.gate.read().await
    }

    /// `write_file`, its turn taken.
    async fn write(&self, path: &Path, content: &[u8]) -> Result<()> {
        self.parent_shown(path).await?;
        self.book.remember(path).await;
        let mode = self.file_mode(path).await;
        self.inner.write_file(path, content).await?;
        self.keep_mode(path, mode).await;
        Ok(())
    }

    /// The sandbox moved or copied `from` to `to`, where it shows `mode`:
    /// kept for applying where that mode is `from`'s own
    /// ([`Book::own_mode`]), and none is kept where it is bashkit's.
    async fn bring(&self, from: &Path, to: &Path, mode: u32) {
        if self.book.own_mode(from).await {
            self.book.bring_mode(to, mode).await;
        } else {
            self.book.modes.forget(&normalize(to));
        }
    }

    /// Each path at and under `to`, where a move from `from` put it, with
    /// its mode and where it came from. bashkit's rename keeps each one's
    /// mode, as `mv` does; links and pipes take none.
    async fn moved(&self, from: &Path, to: &Path) -> Vec<(PathBuf, PathBuf, u32)> {
        let mut found = Vec::new();
        let Ok(top) = self.inner.lstat(to).await else {
            return found;
        };
        let mut pending = vec![(PathBuf::new(), top)];
        while let Some((rest, meta)) = pending.pop() {
            let at = to.join(&rest);
            match meta.file_type {
                FileType::File => {}
                FileType::Directory => {
                    for e in self.inner.read_dir(&at).await.unwrap_or_default() {
                        pending.push((rest.join(&e.name), e.metadata));
                    }
                }
                FileType::Symlink | FileType::Fifo => continue,
            }
            found.push((from.join(&rest), at, meta.mode));
        }
        found
    }
}

#[async_trait]
#[warn(clippy::missing_trait_methods)]
impl FileSystemExt for Recorder {
    fn usage(&self) -> FsUsage {
        self.inner.usage()
    }
    async fn mkfifo(&self, path: &Path, mode: u32) -> Result<()> {
        let _turn = self.turn().await;
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
        let _turn = self.turn().await;
        self.write(path, content).await
    }
    async fn append_file(&self, path: &Path, content: &[u8]) -> Result<()> {
        let _turn = self.turn().await;
        // bashkit's overlay makes a file on append only where its upper
        // layer holds the directory, and never where the sandbox removed
        // one: `>>` onto a new file in a directory only the disk holds, or
        // onto one `rm` took, failed. Appending to nothing is writing, and a
        // write checks that the sandbox shows the parent, as bash does.
        if !self.inner.exists(path).await.unwrap_or(true) {
            return self.write(path, content).await;
        }
        self.book.remember(path).await;
        let mode = self.file_mode(path).await;
        self.inner.append_file(path, content).await?;
        self.keep_mode(path, mode).await;
        Ok(())
    }
    async fn mkdir(&self, path: &Path, recursive: bool) -> Result<()> {
        let _turn = self.turn().await;
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
        let _turn = self.turn().await;
        self.book.remember(path).await;
        self.inner.remove(path, recursive).await?;
        self.book.modes.gone(path);
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
        let _turn = self.turn().await;
        // bashkit's overlay moves a file by writing `to` and then removing
        // `from`: onto itself (`mv a ./a`, `mv d/a d`, a renaming loop over
        // names already right) that removed the file, and applying deleted
        // it from the disk. A rename onto itself does nothing, as in bash.
        // A directory onto itself already fails in bashkit, as in bash.
        if normalize(from) == normalize(to) && !self.inner.lstat(from).await?.file_type.is_dir() {
            return Ok(());
        }
        self.parent_shown(to).await?;
        self.book.remember(from).await;
        self.book.remember(to).await;
        self.inner.rename(from, to).await?;
        self.book.carry(from, to);
        let moved = self.moved(from, to).await;
        for (was, at, mode) in moved {
            self.bring(&was, &at, mode).await;
        }
        self.book.modes.gone(from);
        // A directory moves as a whole: its old place and its new one each
        // record only the directory.
        self.book.wipe(from);
        self.book.wipe(to);
        Ok(())
    }
    async fn copy(&self, from: &Path, to: &Path) -> Result<()> {
        let _turn = self.turn().await;
        self.parent_shown(to).await?;
        self.book.remember(to).await;
        // `cp` keeps the mode of a file it copies onto, and gives a new one
        // its source's; bashkit's copy makes the file 0644 either way.
        let onto = self.file_mode(to).await;
        let source = self.file_mode(from).await;
        self.inner.copy(from, to).await?;
        self.book.carry(from, to);
        match (onto, source) {
            (Some(_), _) => self.keep_mode(to, onto).await,
            (None, Some(mode)) => {
                self.keep_mode(to, source).await;
                self.bring(from, to, mode).await;
            }
            // A link, copied as a link.
            (None, None) => {}
        }
        Ok(())
    }
    async fn symlink(&self, target: &Path, link: &Path) -> Result<()> {
        let _turn = self.turn().await;
        self.parent_shown(link).await?;
        self.book.remember(link).await;
        self.inner.symlink(target, link).await
    }
    async fn read_link(&self, path: &Path) -> Result<PathBuf> {
        self.inner.read_link(path).await
    }
    async fn chmod(&self, path: &Path, mode: u32) -> Result<()> {
        let _turn = self.turn().await;
        self.book.remember(path).await;
        self.inner.chmod(path, mode).await?;
        self.book.set_mode(path, mode).await;
        Ok(())
    }
    async fn set_modified_time(&self, path: &Path, time: SystemTime) -> Result<()> {
        let _turn = self.turn().await;
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
