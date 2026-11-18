//! The test workspace's files as the tools see them: a [`MemFs`] that notes
//! which write a tool asked for, a plain one or a whole one.

use std::{
    io,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use async_trait::async_trait;
use haest_core::fs::{DirEntry, Fs, FsLock, FsStat, MemFs};

/// Which write a tool asked the files for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Write {
    /// [`Fs::write`]: made, or emptied and written.
    Plain,
    /// [`Fs::write_whole`]: replaced whole or not at all.
    Whole,
}

/// Each write a tool asked for, in order, with its path.
pub(crate) type Writes = Arc<Mutex<Vec<(Write, PathBuf)>>>;

/// `mem`, with each write noted in `writes`.
pub(super) struct Noted {
    pub(super) mem: MemFs,
    pub(super) writes: Writes,
}

impl Noted {
    fn note(&self, write: Write, path: &Path) {
        self.writes
            .lock()
            .unwrap()
            .push((write, path.to_path_buf()));
    }
}

#[warn(clippy::missing_trait_methods)]
#[async_trait]
impl Fs for Noted {
    async fn read(&self, path: &Path) -> io::Result<Vec<u8>> {
        self.mem.read(path).await
    }
    async fn read_to_string(&self, path: &Path) -> io::Result<String> {
        self.mem.read_to_string(path).await
    }
    async fn read_head(&self, path: &Path, max: usize) -> io::Result<Vec<u8>> {
        self.mem.read_head(path, max).await
    }
    async fn write(&self, path: &Path, data: &[u8]) -> io::Result<()> {
        self.note(Write::Plain, path);
        self.mem.write(path, data).await
    }
    async fn write_whole(&self, path: &Path, data: &[u8]) -> io::Result<()> {
        self.note(Write::Whole, path);
        self.mem.write_whole(path, data).await
    }
    async fn append(&self, path: &Path, data: &[u8]) -> io::Result<()> {
        self.mem.append(path, data).await
    }
    async fn exists(&self, path: &Path) -> bool {
        self.mem.exists(path).await
    }
    async fn stat(&self, path: &Path) -> io::Result<FsStat> {
        self.mem.stat(path).await
    }
    async fn read_link(&self, path: &Path) -> io::Result<PathBuf> {
        self.mem.read_link(path).await
    }
    async fn read_dir(&self, path: &Path) -> io::Result<Vec<DirEntry>> {
        self.mem.read_dir(path).await
    }
    async fn create_dir(&self, path: &Path) -> io::Result<()> {
        self.mem.create_dir(path).await
    }
    async fn create_private_dir(&self, path: &Path) -> io::Result<()> {
        self.mem.create_private_dir(path).await
    }
    async fn create_dir_all(&self, path: &Path) -> io::Result<()> {
        self.mem.create_dir_all(path).await
    }
    async fn remove(&self, path: &Path) -> io::Result<()> {
        self.mem.remove(path).await
    }
    async fn remove_all(&self, path: &Path) -> io::Result<()> {
        self.mem.remove_all(path).await
    }
    async fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
        self.mem.rename(from, to).await
    }
    async fn chmod(&self, path: &Path, mode: u32) -> io::Result<()> {
        self.mem.chmod(path, mode).await
    }
    async fn try_lock(&self, path: &Path) -> io::Result<Option<FsLock>> {
        self.mem.try_lock(path).await
    }
}
