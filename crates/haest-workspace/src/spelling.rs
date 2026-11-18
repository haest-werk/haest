//! FINDING A PROJECT PATH ON THE REAL DISK ONLY AS THE DISK SPELLS IT.
//!
//! macOS's default disk reads `Lib` as the directory `lib`, and APFS, the
//! case-sensitive kind too, reads `café` in either Unicode normalization as
//! one name. The sandbox tells every spelling apart. Where the disk holds a
//! name only under another spelling, the project overlay's lower layer
//! (`sandbox/spelled_fs.rs`) and the change ledger (`overlay/book.rs`) read
//! the path as absent, so the sandbox shows what applying writes.
//!
//! One `realpath` answers most lookups: on macOS it returns the disk's own
//! spelling. Only when that differs (another spelling, or a symlink on the
//! way) are the directories' listings read. On Linux, `realpath` keeps the
//! spelling it is given, so a case-folding directory there (`chattr +F`)
//! still reads a name the way the disk finds it.

use std::{
    ffi::OsStr,
    io,
    path::{Component, Path, PathBuf},
};

/// A project directory on the real disk, asked how the disk spells a path
/// inside it.
#[derive(Debug, Clone)]
pub(crate) struct Spelling {
    /// The directory as mounted, and as `realpath` spells it; None when it
    /// does not resolve, and then every path reads as asked.
    root: Option<(PathBuf, PathBuf)>,
}

/// How the disk spells a project path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Spelled {
    /// As asked, or the path is not there at all.
    AsAsked,
    /// Under another spelling: that one, as a project path (`/lib/a.rs`), or
    /// None when it resolves outside the project (through a symlink).
    Other(Option<PathBuf>),
}

impl Spelling {
    /// The project at `dir`, resolved once, now. The directory's own spelling
    /// is the human's choice and is never questioned.
    pub(crate) fn of(dir: &Path) -> Self {
        let real = std::fs::canonicalize(dir).ok();
        Self {
            root: real.map(|real| (dir.to_path_buf(), real)),
        }
    }

    /// How the disk spells `path`, a project path (`/Lib/a.rs`). A path the
    /// disk cannot resolve (not there, unreadable) reads as asked: the call
    /// that follows answers for it.
    pub(crate) async fn spelled(&self, path: &Path) -> Spelled {
        self.find(path).await.unwrap_or(Spelled::AsAsked)
    }

    /// How the disk spells the way to `path`, a project path that is not
    /// there: the nearest directory above it that is. A write to `path`
    /// lands in that directory, so where the disk holds it under another
    /// spelling (`lib` for `/Lib/new.rs`), the write lands there too.
    pub(crate) async fn spelled_way(&self, path: &Path) -> Spelled {
        let mut at = path.parent();
        while let Some(dir) = at.filter(|d| d.parent().is_some()) {
            if let Some(spelled) = self.find(dir).await {
                return spelled;
            }
            at = dir.parent();
        }
        Spelled::AsAsked
    }

    /// How the disk spells `path`; None when nothing is there by any
    /// spelling.
    async fn find(&self, path: &Path) -> Option<Spelled> {
        let Some((dir, real)) = &self.root else {
            return Some(Spelled::AsAsked);
        };
        let rel = path.strip_prefix("/").unwrap_or(path);
        // Overlay paths come normalized; anything else is the call's to answer.
        if !rel.components().all(|c| matches!(c, Component::Normal(_))) {
            return Some(Spelled::AsAsked);
        }
        let found = match tokio::fs::canonicalize(dir.join(rel)).await {
            Ok(found) => found,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return None,
            Err(_) => return Some(Spelled::AsAsked),
        };
        if found == real.join(rel) || listed(dir, rel).await {
            return Some(Spelled::AsAsked);
        }
        let inside = found.strip_prefix(real).ok();
        Some(Spelled::Other(inside.map(|r| Path::new("/").join(r))))
    }
}

/// Whether each name in `rel` is one its directory lists, as written. A
/// directory that cannot be read leaves the answer to the call (true).
async fn listed(dir: &Path, rel: &Path) -> bool {
    let mut at = dir.to_path_buf();
    for name in rel {
        if !lists(&at, name).await {
            return false;
        }
        at.push(name);
    }
    true
}

async fn lists(dir: &Path, name: &OsStr) -> bool {
    let Ok(mut entries) = tokio::fs::read_dir(dir).await else {
        return true;
    };
    loop {
        match entries.next_entry().await {
            Ok(Some(e)) if e.file_name() == name => return true,
            Ok(Some(_)) => {}
            Ok(None) => return false,
            Err(_) => return true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_path_reads_as_asked_only_by_the_disks_own_spelling() {
        let t = tempfile::tempdir().unwrap();
        let dir = t.path();
        std::fs::create_dir(dir.join("lib")).unwrap();
        std::fs::write(dir.join("lib/caf\u{e9}"), "").unwrap();
        std::os::unix::fs::symlink("lib", dir.join("link")).unwrap();
        let s = Spelling::of(dir);
        let asked_as_spelled = [
            "/",
            "/lib",
            "/lib/caf\u{e9}",
            "/link/caf\u{e9}",
            "/nope",
            "/lib/../x",
        ];
        for asked in asked_as_spelled {
            let got = s.spelled(Path::new(asked)).await;
            assert_eq!(got, Spelled::AsAsked, "{asked}");
        }
        // Each other spelling, where this disk reads it as the same name.
        for asked in ["/LIB/caf\u{e9}", "/lib/cafe\u{301}"] {
            let want = if dir.join(&asked[1..]).exists() {
                Spelled::Other(Some(PathBuf::from("/lib/caf\u{e9}")))
            } else {
                Spelled::AsAsked
            };
            assert_eq!(s.spelled(Path::new(asked)).await, want, "{asked}");
        }
    }

    /// A path that is not there lands in the nearest directory above it that
    /// is, as the disk spells that one.
    #[tokio::test]
    async fn the_way_to_a_missing_path_is_its_nearest_directory_there() {
        let t = tempfile::tempdir().unwrap();
        let dir = t.path();
        std::fs::create_dir(dir.join("lib")).unwrap();
        let s = Spelling::of(dir);
        for asked in ["/lib/new.rs", "/lib/new/deeper/x.rs", "/nope/x.rs", "/x.rs"] {
            let got = s.spelled_way(Path::new(asked)).await;
            assert_eq!(got, Spelled::AsAsked, "{asked}");
        }
        let want = if dir.join("LIB").exists() {
            Spelled::Other(Some(PathBuf::from("/lib")))
        } else {
            Spelled::AsAsked
        };
        for asked in ["/LIB/new.rs", "/LIB/new/deeper/x.rs"] {
            assert_eq!(s.spelled_way(Path::new(asked)).await, want, "{asked}");
        }
    }

    #[tokio::test]
    async fn a_directory_that_does_not_resolve_reads_every_path_as_asked() {
        let s = Spelling::of(Path::new("/no/such/dir"));
        assert_eq!(s.spelled(Path::new("/LIB")).await, Spelled::AsAsked);
    }
}
