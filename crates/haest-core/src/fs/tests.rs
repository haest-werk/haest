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

/// Every path under `/` with its kind and, for a file, its text, walked as a
/// reader sees the tree: through `read_dir` and `read`.
async fn tree(fs: &MemFs) -> Vec<(PathBuf, Option<String>)> {
    let mut out = Vec::new();
    let mut dirs = vec![PathBuf::from("/")];
    while let Some(dir) = dirs.pop() {
        for e in fs.read_dir(&dir).await.unwrap() {
            let p = dir.join(&e.name);
            if e.kind == FileKind::Dir {
                dirs.push(p.clone());
                out.push((p, None));
            } else {
                let text = fs.read_to_string(&p).await.unwrap();
                out.push((p, Some(text)));
            }
        }
    }
    out.sort();
    out
}

/// What rename(2) refuses, MemFs refuses, and nothing moves: a file onto a
/// directory (EISDIR), a directory onto a file (ENOTDIR), a directory onto
/// one with anything in it (ENOTEMPTY), `/` and its own parent included.
/// It replaced whatever was at `to`: a directory moved onto a non-empty one
/// merged the two, its files over the same-named ones there, and a file
/// moved onto a directory left the directory's files under a file.
#[tokio::test]
async fn memfs_rename_refuses_what_rename_2_refuses_and_moves_nothing() {
    use io::ErrorKind::{DirectoryNotEmpty, IsADirectory, NotADirectory};
    let fs = MemFs::from_files([
        ("/p/f.txt", "F"),
        ("/p/d/x.txt", "X"),
        ("/a/one.txt", "1"),
        ("/a/two.txt", "from a"),
        ("/b/two.txt", "from b"),
    ]);
    let before = tree(&fs).await;
    for (from, to, want) in [
        ("/p/f.txt", "/p/d", IsADirectory),
        ("/p/f.txt", "/p", IsADirectory),
        ("/p/f.txt", "/", IsADirectory),
        ("/p/d", "/p/f.txt", NotADirectory),
        ("/a", "/b", DirectoryNotEmpty),
        ("/p/d", "/p", DirectoryNotEmpty),
        ("/a", "/", DirectoryNotEmpty),
    ] {
        let got = fs.rename(Path::new(from), Path::new(to)).await;
        assert_eq!(got.map_err(|e| e.kind()), Err(want), "{from} -> {to}");
        assert_eq!(tree(&fs).await, before, "{from} -> {to} moved something");
    }
    let kept = fs.read_to_string(Path::new("/b/two.txt")).await.unwrap();
    assert_eq!(kept, "from b");
    assert!(fs.read_dir(Path::new("/p/d")).await.is_ok());
}

/// What rename(2) replaces, MemFs replaces: a file at `to` with a file, an
/// empty directory with a directory. Onto itself, nothing changes.
#[tokio::test]
async fn memfs_rename_replaces_a_file_and_an_empty_dir_and_leaves_itself_alone() {
    let fs = MemFs::from_files([("/f", "new"), ("/g", "old"), ("/d/x", "X")]);
    fs.create_dir(Path::new("/empty")).await.unwrap();
    fs.rename(Path::new("/f"), Path::new("/g")).await.unwrap();
    fs.rename(Path::new("/d"), Path::new("/empty"))
        .await
        .unwrap();
    let after = tree(&fs).await;
    let want = [
        (PathBuf::from("/empty"), None),
        (PathBuf::from("/empty/x"), Some("X".to_string())),
        (PathBuf::from("/g"), Some("new".to_string())),
    ];
    assert_eq!(after, want);
    for p in ["/g", "/empty"] {
        fs.rename(Path::new(p), Path::new(p)).await.unwrap();
        assert_eq!(tree(&fs).await, after, "{p} onto itself");
    }
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

/// A store with nothing of its own to add replaces a file whole with its
/// own write, and refuses what its write refuses.
#[tokio::test]
async fn the_default_whole_write_is_the_stores_write() {
    let fs = Minimal(MemFs::from_files([("/p/a.txt", "the old text")]));
    fs.write_whole(Path::new("/p/a.txt"), b"new").await.unwrap();
    assert_eq!(fs.read(Path::new("/p/a.txt")).await.unwrap(), b"new");
    let no_dir = fs.write_whole(Path::new("/q/a.txt"), b"x").await;
    assert_eq!(no_dir.map_err(|e| e.kind()), Err(io::ErrorKind::NotFound));
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
