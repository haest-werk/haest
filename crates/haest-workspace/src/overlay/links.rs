//! Whether a project path's real location is `root/path`. A symlink on the way
//! (`vendor -> ~/.ssh`, or `hosts -> /etc/hosts` itself) would take a write or
//! a delete wherever it points, and applying never reaches outside the project
//! or through a link: such a path is refused instead (`apply.rs`).

use std::{
    collections::HashMap,
    path::{Component, Path, PathBuf},
};

use haest_core::fs::{FileKind, Fs};

/// Looks up each component on the real disk, once per scan.
pub(crate) struct Links<'a> {
    disk: &'a dyn Fs,
    root: &'a Path,
    /// A real path seen: Some(true) a symlink, Some(false) something else,
    /// None not there.
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
            let parent = at.clone();
            at.push(name);
            let link = match self.seen.get(&at) {
                Some(&seen) => seen,
                None => {
                    // `stat` follows links; a listing names one for what it is.
                    let name = name.to_string_lossy();
                    let kind = self.disk.read_dir(&parent).await.ok().and_then(|entries| {
                        entries.into_iter().find(|e| e.name == name).map(|e| e.kind)
                    });
                    let seen = kind.map(|k| k == FileKind::Symlink);
                    self.seen.insert(at.clone(), seen);
                    seen
                }
            };
            match link {
                Some(true) => return true,
                Some(false) => continue,
                // Not there: nothing below it is either.
                None => return false,
            }
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use haest_core::fs::MemFs;

    #[tokio::test]
    async fn a_plain_path_is_not_through_a_link_present_or_not() {
        let fs = MemFs::with_files([("/p/src/a.rs", "x")]);
        let mut links = Links::new(&fs, Path::new("/p"));
        assert!(!links.through_a_link(Path::new("/src/a.rs")).await);
        assert!(!links.through_a_link(Path::new("/src/new/b.rs")).await);
        assert!(!links.through_a_link(Path::new("/nope/c.rs")).await);
    }
}
