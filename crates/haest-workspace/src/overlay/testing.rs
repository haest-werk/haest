//! A real disk for the overlay's unit tests: a `MemFs` a test can watch, and
//! move under an apply the way the human would.

use std::{
    collections::HashMap,
    io,
    path::{Path, PathBuf},
    sync::Mutex,
};

use async_trait::async_trait;
use haest_core::fs::{DirEntry, Fs, FsStat, MemFs};

/// `MemFs` as the real disk (or the overlay's files). It notes each path
/// asked whether it is a link, and each directory listed, and plays the
/// human: a path handed to [`edit_on_read`](Self::edit_on_read) is rewritten
/// (or [deleted](Self::delete_on_read)) right after its next read has
/// returned.
#[derive(Default)]
pub(super) struct Disk {
    pub(super) inner: MemFs,
    pub(super) asked: Mutex<Vec<PathBuf>>,
    pub(super) listed: Mutex<Vec<PathBuf>>,
    /// What the human does to each path after its next read: save this
    /// content, or (None) delete it.
    edits: Mutex<HashMap<PathBuf, Option<Vec<u8>>>>,
}

impl Disk {
    /// The next read of `path` returns what is there, then the human saves
    /// `content` over it.
    pub(super) fn edit_on_read(&self, path: impl Into<PathBuf>, content: &[u8]) {
        let mut edits = self.edits.lock().unwrap();
        edits.insert(path.into(), Some(content.to_vec()));
    }

    /// The next read of `path` returns what is there, then the human
    /// deletes it.
    pub(super) fn delete_on_read(&self, path: impl Into<PathBuf>) {
        self.edits.lock().unwrap().insert(path.into(), None);
    }
}

#[async_trait]
impl Fs for Disk {
    async fn read(&self, p: &Path) -> io::Result<Vec<u8>> {
        let read = self.inner.read(p).await;
        let edit = self.edits.lock().unwrap().remove(p);
        match edit {
            Some(Some(content)) => self.inner.write(p, &content).await?,
            Some(None) => self.inner.remove(p).await?,
            None => {}
        }
        read
    }
    async fn write(&self, p: &Path, d: &[u8]) -> io::Result<()> {
        self.inner.write(p, d).await
    }
    async fn append(&self, p: &Path, d: &[u8]) -> io::Result<()> {
        self.inner.append(p, d).await
    }
    async fn stat(&self, p: &Path) -> io::Result<FsStat> {
        self.inner.stat(p).await
    }
    async fn read_link(&self, p: &Path) -> io::Result<PathBuf> {
        self.asked.lock().unwrap().push(p.to_path_buf());
        self.inner.read_link(p).await
    }
    async fn read_dir(&self, p: &Path) -> io::Result<Vec<DirEntry>> {
        self.listed.lock().unwrap().push(p.to_path_buf());
        self.inner.read_dir(p).await
    }
    async fn create_dir(&self, p: &Path) -> io::Result<()> {
        self.inner.create_dir(p).await
    }
    async fn create_dir_all(&self, p: &Path) -> io::Result<()> {
        self.inner.create_dir_all(p).await
    }
    async fn remove(&self, p: &Path) -> io::Result<()> {
        self.inner.remove(p).await
    }
    async fn remove_all(&self, p: &Path) -> io::Result<()> {
        self.inner.remove_all(p).await
    }
    async fn rename(&self, a: &Path, b: &Path) -> io::Result<()> {
        self.inner.rename(a, b).await
    }
}
