//! Whether a project path's real location is `root/path`. A symlink on the way
//! (`vendor -> ~/.ssh`, or `hosts -> /etc/hosts` itself) would take a write or
//! a delete wherever it points, and applying never reaches outside the project
//! or through a link: such a path is refused instead (`apply.rs`).

use std::{
    collections::HashMap,
    path::{Component, Path, PathBuf},
};

use haest_core::fs::{FileKind, Fs};

/// Looks up each component on the real disk, listing each real directory on
/// the way at most once: one serves a whole apply. Applying never makes a
/// link, so nothing it writes turns a remembered answer into a link; a link
/// someone else makes mid-apply is a race a fresh lookup per change would not
/// close either.
pub(crate) struct Links<'a> {
    disk: &'a dyn Fs,
    root: &'a Path,
    /// Each real directory listed: its entries by name, true for a symlink;
    /// None when it could not be listed (not there, or not a directory).
    listed: HashMap<PathBuf, Option<HashMap<String, bool>>>,
}

impl<'a> Links<'a> {
    pub(crate) fn new(disk: &'a dyn Fs, root: &'a Path) -> Self {
        Self {
            disk,
            root,
            listed: HashMap::new(),
        }
    }

    /// True when a component of `path` (an overlay path, `/vendor/a`) is a
    /// symlink on the real disk, the last one included. The root itself is
    /// the human's choice and is not checked.
    pub(crate) async fn through_a_link(&mut self, path: &Path) -> bool {
        let mut at = self.root.to_path_buf();
        for part in path.components() {
            let Component::Normal(name) = part else {
                continue;
            };
            match self.entry(&at, &name.to_string_lossy()).await {
                Some(true) => return true,
                Some(false) => at.push(name),
                // Not there: nothing below it is either.
                None => return false,
            }
        }
        false
    }

    /// What `name` in the real directory `dir` is: Some(true) a symlink,
    /// Some(false) something else, None not there.
    async fn entry(&mut self, dir: &Path, name: &str) -> Option<bool> {
        if !self.listed.contains_key(dir) {
            // `stat` follows links; a listing names one for what it is.
            let entries = self.disk.read_dir(dir).await.ok().map(|entries| {
                entries
                    .into_iter()
                    .map(|e| (e.name, e.kind == FileKind::Symlink))
                    .collect()
            });
            self.listed.insert(dir.to_path_buf(), entries);
        }
        let entries = self.listed.get(dir)?.as_ref()?;
        entries.get(name).copied()
    }
}

#[cfg(test)]
mod tests {
    use std::{
        io,
        sync::{Arc, Mutex},
    };

    use async_trait::async_trait;
    use haest_core::{
        fs::{DirEntry, FsRef, FsStat, MemFs},
        workspace::ChangeSet,
    };

    use super::*;
    use crate::{bridge::BashkitFsOverHaest, overlay::OverlayChanges};

    /// `MemFs`, noting each directory it lists.
    #[derive(Default)]
    struct Listings {
        inner: MemFs,
        listed: Mutex<Vec<PathBuf>>,
    }

    #[async_trait]
    impl Fs for Listings {
        async fn read(&self, p: &Path) -> io::Result<Vec<u8>> {
            self.inner.read(p).await
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

    /// Each change built its own lookup, which lists a component's parent to
    /// learn what the component is: an apply of many files in one directory
    /// listed that directory, and each above it, once per file.
    #[tokio::test]
    async fn an_apply_lists_each_real_directory_on_the_way_once() {
        let disk = Arc::new(Listings::default());
        disk.inner
            .create_dir_all(Path::new("/p/src"))
            .await
            .unwrap();
        let mem = MemFs::new();
        mem.create_dir_all(Path::new("/src")).await.unwrap();
        let overlay = Arc::new(BashkitFsOverHaest::new(FsRef::new(mem)));
        let changes = OverlayChanges::new(overlay, disk.clone(), "/p");
        let recorder = changes.recorder();
        for name in ["a.rs", "b.rs", "c.rs"] {
            let path = Path::new("/src").join(name);
            recorder.write_file(&path, b"x").await.unwrap();
        }

        let report = changes.apply().await;
        assert_eq!(report.applied.len(), 3, "{report:?}");
        let listed = disk.listed.lock().unwrap();
        for dir in ["/p", "/p/src"] {
            let times = listed.iter().filter(|d| d.as_path() == Path::new(dir));
            assert_eq!(times.count(), 1, "{dir}: {listed:?}");
        }
    }

    #[tokio::test]
    async fn a_plain_path_is_not_through_a_link_present_or_not() {
        let fs = MemFs::from_files([("/p/src/a.rs", "x")]);
        let mut links = Links::new(&fs, Path::new("/p"));
        assert!(!links.through_a_link(Path::new("/src/a.rs")).await);
        assert!(!links.through_a_link(Path::new("/src/new/b.rs")).await);
        assert!(!links.through_a_link(Path::new("/nope/c.rs")).await);
    }
}
