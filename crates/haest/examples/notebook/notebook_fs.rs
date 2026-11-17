//! The notebook's file system, faked: the notebook as the node lets the asker
//! see it, and every change the run makes STAGED for the asker -- nothing
//! reaches the notebook while the run works; the asker reviews the changes and
//! applies them.
//! Read, Write, Edit and the sandboxed Bash all go through it, so the file the
//! agent writes is the file `cat` shows.
//!
//! PLUG POINT: `haest::Fs`, under `SandboxWorkspace::over`.

use std::{
    collections::BTreeSet,
    io,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, MutexGuard},
};

use haest::{DirEntry, Fs, FsStat, MemFs, async_trait};

/// A cheap `Clone` whose clones are the one notebook: the workspace gets
/// one, the run reads the staged changes from another.
#[derive(Clone)]
pub(crate) struct NotebookFs {
    /// What the run sees: the notebook, plus what the run changed.
    view: MemFs,
    /// Every path the run changed: the review the asker gets.
    staged: Arc<Mutex<BTreeSet<PathBuf>>>,
}

impl NotebookFs {
    /// The notebook as the asker may see it.
    pub(crate) fn new(files: &[(&str, &str)]) -> Self {
        Self {
            view: MemFs::from_files(files.iter().copied()),
            staged: Arc::default(),
        }
    }

    /// The staged changes, path and new content, for the asker to review.
    pub(crate) async fn staged(&self) -> Vec<(PathBuf, String)> {
        let paths: Vec<PathBuf> = self.paths().iter().cloned().collect();
        let mut out = Vec::new();
        for p in paths {
            let text = self.view.read_to_string(&p).await.unwrap_or_default();
            out.push((p, text));
        }
        out
    }

    fn stage(&self, path: &Path) {
        self.paths().insert(path.to_path_buf());
    }

    /// Only this file's code runs under the lock, so a poisoned one is a bug here.
    fn paths(&self) -> MutexGuard<'_, BTreeSet<PathBuf>> {
        self.staged.lock().expect("the staged paths' lock poisoned")
    }
}

#[async_trait]
impl Fs for NotebookFs {
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
    async fn create_dir(&self, path: &Path) -> io::Result<()> {
        self.view.create_dir(path).await
    }
    async fn create_dir_all(&self, path: &Path) -> io::Result<()> {
        self.view.create_dir_all(path).await
    }
    async fn remove(&self, path: &Path) -> io::Result<()> {
        self.view.remove(path).await?;
        self.stage(path);
        Ok(())
    }
    async fn remove_all(&self, path: &Path) -> io::Result<()> {
        self.view.remove_all(path).await?;
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
