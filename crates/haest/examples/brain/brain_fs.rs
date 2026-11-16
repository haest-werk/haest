//! brain's BrainFs, faked: the brain as the node lets the asker see it, and
//! every change the run makes STAGED for the asker -- nothing reaches the brain
//! while the run works; the asker reviews the changes and applies them.
//! Read, Write, Edit and the sandboxed Bash all go through it, so the file the
//! agent writes is the file `cat` shows.
//!
//! PLUG POINT: `haest::fs::Fs`, under `SandboxWorkspace::over`.

use std::{
    collections::BTreeSet,
    io,
    path::{Path, PathBuf},
    sync::Mutex,
};

use async_trait::async_trait;
use haest::fs::{DirEntry, Fs, FsStat, MemFs};

pub struct BrainFs {
    /// What the run sees: the brain, plus what the run changed.
    view: MemFs,
    /// Every path the run changed: the review the asker gets.
    staged: Mutex<BTreeSet<PathBuf>>,
}

impl BrainFs {
    /// The brain as the asker may see it.
    pub fn new(files: &[(&str, &str)]) -> Self {
        Self {
            view: MemFs::with_files(files.iter().copied()),
            staged: Mutex::default(),
        }
    }

    /// The staged changes, path and new content, for the asker to review.
    pub async fn staged(&self) -> Vec<(PathBuf, String)> {
        let paths: Vec<PathBuf> = self.staged.lock().unwrap().iter().cloned().collect();
        let mut out = Vec::new();
        for p in paths {
            let text = self.view.read_to_string(&p).await.unwrap_or_default();
            out.push((p, text));
        }
        out
    }

    fn stage(&self, path: &Path) {
        self.staged.lock().unwrap().insert(path.to_path_buf());
    }
}

#[async_trait]
impl Fs for BrainFs {
    async fn read(&self, path: &Path) -> io::Result<Vec<u8>> {
        self.view.read(path).await
    }
    async fn write(&self, path: &Path, data: &[u8]) -> io::Result<()> {
        self.view.write(path, data).await?;
        self.stage(path);
        Ok(())
    }
    async fn append(&self, path: &Path, data: &[u8]) -> io::Result<()> {
        self.view.append(path, data).await?;
        self.stage(path);
        Ok(())
    }
    async fn stat(&self, path: &Path) -> io::Result<FsStat> {
        self.view.stat(path).await
    }
    async fn read_dir(&self, path: &Path) -> io::Result<Vec<DirEntry>> {
        self.view.read_dir(path).await
    }
    async fn mkdir(&self, path: &Path, recursive: bool) -> io::Result<()> {
        self.view.mkdir(path, recursive).await
    }
    async fn remove(&self, path: &Path, recursive: bool) -> io::Result<()> {
        self.view.remove(path, recursive).await?;
        self.stage(path);
        Ok(())
    }
    async fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
        self.view.rename(from, to).await?;
        self.stage(from);
        self.stage(to);
        Ok(())
    }
}
