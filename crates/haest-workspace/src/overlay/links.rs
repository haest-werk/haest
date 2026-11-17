//! Whether a project path's real location is `root/path`. A symlink on the way
//! (`vendor -> ~/.ssh`, or `hosts -> /etc/hosts` itself) would take a write or
//! a delete wherever it points, and applying never reaches outside the project
//! or through a link: such a path is refused instead (`apply.rs`).

use std::{
    collections::HashMap,
    io,
    path::{Component, Path, PathBuf},
};

use haest_core::fs::Fs;

/// Asks the real disk about each component of a path, asking each real path
/// on the way at most once: one serves a whole apply. Applying never makes a
/// link, so nothing it writes turns a remembered answer into a link; a link
/// someone else makes mid-apply is a race a fresh lookup per change would not
/// close either.
pub(crate) struct Links<'a> {
    disk: &'a dyn Fs,
    root: &'a Path,
    /// Each real path asked, as spelled: Some(true) a symlink, Some(false)
    /// something else, None not there.
    seen: HashMap<PathBuf, Option<bool>>,
}

impl<'a> Links<'a> {
    pub(crate) fn new(disk: &'a dyn Fs, root: &'a Path) -> Self {
        Self {
            disk,
            root,
            seen: HashMap::new(),
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
            at.push(name);
            let link = match self.seen.get(&at) {
                Some(&seen) => seen,
                None => {
                    let seen = self.kind_of(&at).await;
                    self.seen.insert(at.clone(), seen);
                    seen
                }
            };
            match link {
                Some(true) => return true,
                Some(false) => {}
                // Not there: nothing below it is either.
                None => return false,
            }
        }
        false
    }

    /// What is at `at`, asked of the disk by the path itself, so the disk's
    /// own rules for names hold: where it matches names without case or
    /// Unicode normalization (macOS's default), `Vendor` is the link
    /// `vendor`, which a lookup in the parent's listing would miss. `stat`
    /// follows links; `read_link` does not.
    async fn kind_of(&self, at: &Path) -> Option<bool> {
        match self.disk.read_link(at).await {
            Ok(_) => Some(true),
            Err(e)
                if matches!(
                    e.kind(),
                    io::ErrorKind::NotFound | io::ErrorKind::NotADirectory
                ) =>
            {
                None
            }
            // There and not a link (`InvalidInput`), or an answer that says
            // neither (a permission): the components below it are still asked.
            Err(_) => Some(false),
        }
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

    /// `MemFs`, noting each path asked whether it is a link.
    #[derive(Default)]
    struct Asked {
        inner: MemFs,
        asked: Mutex<Vec<PathBuf>>,
    }

    #[async_trait]
    impl Fs for Asked {
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
        async fn read_link(&self, p: &Path) -> io::Result<PathBuf> {
            self.asked.lock().unwrap().push(p.to_path_buf());
            self.inner.read_link(p).await
        }
        async fn read_dir(&self, p: &Path) -> io::Result<Vec<DirEntry>> {
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

    /// Each change built its own lookup: an apply of many files in one
    /// directory asked the disk about that directory, and each above it, once
    /// per file.
    #[tokio::test]
    async fn an_apply_asks_about_each_real_directory_on_the_way_once() {
        let disk = Arc::new(Asked::default());
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
        // The files are asked by the scan too; only the apply asks the way.
        let asked = disk.asked.lock().unwrap();
        let times = asked.iter().filter(|p| p.as_path() == Path::new("/p/src"));
        assert_eq!(times.count(), 1, "{asked:?}");
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
