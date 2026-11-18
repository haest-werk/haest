use std::{
    io,
    path::{Path, PathBuf},
    sync::Mutex,
};

use super::*;

#[tokio::test]
async fn memfs_reads_what_it_wrote_and_lists_direct_children_only() {
    let fs = MemFs::from_files([("/p/a.txt", "A"), ("/p/sub/b.txt", "B")]);
    assert_eq!(fs.read_to_string(Path::new("/p/a.txt")).await.unwrap(), "A");
    let head = |max| fs.read_head(Path::new("/p/a.txt"), max);
    assert_eq!(
        (head(0).await.unwrap(), head(9).await.unwrap()),
        (vec![], b"A".to_vec())
    );
    assert!(fs.read_head(Path::new("/p/sub"), 1).await.is_err());
    let names: Vec<_> = fs
        .read_dir(Path::new("/p"))
        .await
        .unwrap()
        .into_iter()
        .map(|e| e.name)
        .collect();
    assert_eq!(names, ["a.txt", "sub"]);
    assert!(fs.write(Path::new("/nope/x"), b"x").await.is_err());
}

#[tokio::test]
async fn memfs_rename_moves_a_whole_tree_and_remove_needs_recursive() {
    let fs = MemFs::from_files([("/d/x/1", "1")]);
    fs.rename(Path::new("/d/x"), Path::new("/d/y"))
        .await
        .unwrap();
    assert!(fs.exists(Path::new("/d/y/1")).await);
    assert!(!fs.exists(Path::new("/d/x")).await);
    assert!(fs.remove(Path::new("/d")).await.is_err());
    fs.remove_all(Path::new("/d")).await.unwrap();
    assert!(!fs.exists(Path::new("/d/y/1")).await);
}

/// What a real fs refuses: a dir moved into itself (EINVAL), the root removed.
#[tokio::test]
async fn memfs_refuses_a_dir_into_itself_and_removing_the_root() {
    let fs = MemFs::from_files([("/a/x", "1")]);
    let err = fs.rename(Path::new("/a"), Path::new("/a/b")).await;
    assert_eq!(err.map_err(|e| e.kind()), Err(io::ErrorKind::InvalidInput));
    assert!(fs.exists(Path::new("/a/x")).await, "nothing moved");
    assert!(fs.remove(Path::new("/")).await.is_err());
    assert!(fs.remove_all(Path::new("/")).await.is_err());
    fs.write(Path::new("/new"), b"n").await.unwrap();
    assert!(fs.exists(Path::new("/a/x")).await);
}

/// The default `read_link`: a file system without links has none, and a
/// missing path is still missing.
#[tokio::test]
async fn a_file_system_without_links_says_no_path_is_one() {
    use io::ErrorKind::{InvalidInput, NotFound};
    let fs = MemFs::from_files([("/d/a", "1")]);
    for (p, want) in [
        ("/d/a", InvalidInput),
        ("/d", InvalidInput),
        ("/nope", NotFound),
    ] {
        let got = fs.read_link(Path::new(p)).await.unwrap_err().kind();
        assert_eq!(got, want, "{p}");
    }
}

#[test]
fn normalize_resolves_dots_lexically() {
    assert_eq!(normalize(Path::new("/a/./b/../c")), PathBuf::from("/a/c"));
    assert_eq!(normalize(Path::new("/../x")), PathBuf::from("/x"));
}

#[test]
fn relative_paths_resolve_from_cwd() {
    assert_eq!(
        resolve_path(Path::new("/p"), "a/../b.txt"),
        PathBuf::from("/p/b.txt")
    );
    assert_eq!(resolve_path(Path::new("/p"), "/abs"), PathBuf::from("/abs"));
}

#[tokio::test]
async fn ensure_dir_makes_the_dir_and_refuses_a_file_in_its_place() {
    let fs = MemFs::from_files([("/p/file", "x")]);
    ensure_dir(&fs, Path::new("/p/a/b")).await.unwrap();
    ensure_dir(&fs, Path::new("/p/a/b")).await.unwrap();
    assert_eq!(
        fs.stat(Path::new("/p/a/b")).await.unwrap().kind,
        FileKind::Dir
    );
    assert!(ensure_dir(&fs, Path::new("/p/file")).await.is_err());
}

/// Only the required methods, over a [`MemFs`]: what a host's own store
/// gets from the default `create_dir_all`, and from [`remove_tree`] as its
/// `remove_all`.
struct Minimal(MemFs);

#[async_trait::async_trait]
impl Fs for Minimal {
    async fn read(&self, p: &Path) -> io::Result<Vec<u8>> {
        self.0.read(p).await
    }
    async fn write(&self, p: &Path, d: &[u8]) -> io::Result<()> {
        self.0.write(p, d).await
    }
    async fn append(&self, p: &Path, d: &[u8]) -> io::Result<()> {
        self.0.append(p, d).await
    }
    async fn stat(&self, p: &Path) -> io::Result<FsStat> {
        self.0.stat(p).await
    }
    async fn read_dir(&self, p: &Path) -> io::Result<Vec<DirEntry>> {
        self.0.read_dir(p).await
    }
    async fn create_dir(&self, p: &Path) -> io::Result<()> {
        self.0.create_dir(p).await
    }
    async fn remove(&self, p: &Path) -> io::Result<()> {
        self.0.remove(p).await
    }
    async fn remove_all(&self, p: &Path) -> io::Result<()> {
        remove_tree(self, p).await
    }
    async fn rename(&self, a: &Path, b: &Path) -> io::Result<()> {
        self.0.rename(a, b).await
    }
}

#[tokio::test]
async fn the_default_create_dir_all_makes_every_parent_and_takes_one_already_there() {
    let fs = Minimal(MemFs::from_files([("/p/file", "x")]));
    fs.create_dir_all(Path::new("/p/a/b/c")).await.unwrap();
    assert!(fs.stat(Path::new("/p/a/b/c")).await.unwrap().is_dir());
    fs.create_dir_all(Path::new("/p/a/b")).await.unwrap();
    let under_a_file = fs.create_dir_all(Path::new("/p/file/d")).await;
    assert_eq!(
        under_a_file.map_err(|e| e.kind()),
        Err(io::ErrorKind::NotADirectory)
    );
}

#[tokio::test]
async fn remove_tree_deletes_a_whole_tree_and_remove_does_not() {
    let fs = Minimal(MemFs::from_files([
        ("/p/a/b/x", "1"),
        ("/p/a/y", "2"),
        ("/p/z", "3"),
    ]));
    assert!(fs.remove(Path::new("/p/a")).await.is_err(), "not empty");
    fs.remove_all(Path::new("/p/a")).await.unwrap();
    assert!(!fs.exists(Path::new("/p/a")).await);
    assert!(fs.exists(Path::new("/p/z")).await, "its sibling stays");
    fs.remove_all(Path::new("/p/z")).await.unwrap();
    let gone = fs.remove_all(Path::new("/p/z")).await;
    assert_eq!(gone.map_err(|e| e.kind()), Err(io::ErrorKind::NotFound));
}

/// A host's store with modes, over a [`MemFs`]: it keeps each mode set,
/// or refuses every one.
struct Moded {
    files: MemFs,
    modes: Mutex<Vec<(PathBuf, u32)>>,
    refuses: bool,
}

impl Moded {
    fn new(refuses: bool) -> Self {
        let files = MemFs::from_files([("/p/file", "x")]);
        let modes = Mutex::new(Vec::new());
        Self {
            files,
            modes,
            refuses,
        }
    }
}

#[async_trait::async_trait]
impl Fs for Moded {
    async fn read(&self, p: &Path) -> io::Result<Vec<u8>> {
        self.files.read(p).await
    }
    async fn write(&self, p: &Path, d: &[u8]) -> io::Result<()> {
        self.files.write(p, d).await
    }
    async fn append(&self, p: &Path, d: &[u8]) -> io::Result<()> {
        self.files.append(p, d).await
    }
    async fn stat(&self, p: &Path) -> io::Result<FsStat> {
        self.files.stat(p).await
    }
    async fn read_dir(&self, p: &Path) -> io::Result<Vec<DirEntry>> {
        self.files.read_dir(p).await
    }
    async fn create_dir(&self, p: &Path) -> io::Result<()> {
        self.files.create_dir(p).await
    }
    async fn remove(&self, p: &Path) -> io::Result<()> {
        self.files.remove(p).await
    }
    async fn remove_all(&self, p: &Path) -> io::Result<()> {
        self.files.remove_all(p).await
    }
    async fn rename(&self, a: &Path, b: &Path) -> io::Result<()> {
        self.files.rename(a, b).await
    }
    async fn chmod(&self, p: &Path, mode: u32) -> io::Result<()> {
        if self.refuses {
            return Err(io::ErrorKind::PermissionDenied.into());
        }
        self.modes.lock().unwrap().push((p.to_path_buf(), mode));
        Ok(())
    }
}

/// The default `create_private_dir`: the directory made and given 0700,
/// a name already taken refused; on a store with no modes, just made.
#[tokio::test]
async fn the_default_private_dir_is_made_owner_only() {
    use io::ErrorKind::{AlreadyExists, NotFound};
    let fs = Moded::new(false);
    fs.create_private_dir(Path::new("/p/d")).await.unwrap();
    assert!(fs.stat(Path::new("/p/d")).await.unwrap().is_dir());
    let modes = fs.modes.lock().unwrap().clone();
    assert_eq!(modes, [(PathBuf::from("/p/d"), 0o700)]);
    for (p, want) in [("/p/d", AlreadyExists), ("/p/file", AlreadyExists)] {
        let got = fs.create_private_dir(Path::new(p)).await.unwrap_err();
        assert_eq!(got.kind(), want, "{p}");
    }
    let no_parent = fs.create_private_dir(Path::new("/q/d")).await;
    assert_eq!(no_parent.map_err(|e| e.kind()), Err(NotFound));
    assert_eq!(fs.modes.lock().unwrap().len(), 1, "only the one made");

    let mem = MemFs::from_files([("/p/file", "x")]);
    mem.create_private_dir(Path::new("/p/d")).await.unwrap();
    assert!(mem.stat(Path::new("/p/d")).await.unwrap().is_dir());
}

/// A directory the store will not make private is not left behind open
/// to others: the default removes it again and says why.
#[tokio::test]
async fn a_private_dir_the_store_cannot_make_private_is_removed_again() {
    let fs = Moded::new(true);
    let refused = fs.create_private_dir(Path::new("/p/d")).await;
    assert_eq!(
        refused.map_err(|e| e.kind()),
        Err(io::ErrorKind::PermissionDenied)
    );
    assert!(!fs.exists(Path::new("/p/d")).await, "left behind");
    assert!(fs.exists(Path::new("/p/file")).await);
}
