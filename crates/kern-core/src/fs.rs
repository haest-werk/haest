//! The file system the agent sees. Every file operation kern performs -- tools,
//! spilled output, the session log, instruction files -- goes through an [`Fs`].
//! Nothing in kern touches `std::fs` for the agent directly.
//!
//! PLUG POINT: a host with its own storage (brain's `BrainFs`) implements [`Fs`];
//! `kern-workspace` can put a sandboxed bash over ANY `Fs`, so the file the agent
//! writes with Write is the file `cat` shows.
//!
//! Paths are absolute. A relative path is the caller's bug ([`crate::tool::resolve_path`]).

use std::{
    collections::BTreeMap,
    io,
    path::{Component, Path, PathBuf},
    sync::Mutex,
    time::SystemTime,
};

use async_trait::async_trait;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileKind {
    File,
    Dir,
    Symlink,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FsStat {
    pub kind: FileKind,
    pub size: u64,
    pub modified: Option<SystemTime>,
}

impl FsStat {
    pub fn is_dir(&self) -> bool {
        self.kind == FileKind::Dir
    }
    pub fn is_file(&self) -> bool {
        self.kind == FileKind::File
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirEntry {
    pub name: String,
    pub kind: FileKind,
}

#[async_trait]
pub trait Fs: Send + Sync {
    async fn read(&self, path: &Path) -> io::Result<Vec<u8>>;
    /// Strict UTF-8; invalid bytes are `InvalidData`.
    async fn read_to_string(&self, path: &Path) -> io::Result<String> {
        let bytes = self.read(path).await?;
        String::from_utf8(bytes).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
    }
    /// Create or replace. The parent directory must exist.
    async fn write(&self, path: &Path, data: &[u8]) -> io::Result<()>;
    async fn append(&self, path: &Path, data: &[u8]) -> io::Result<()>;
    async fn exists(&self, path: &Path) -> bool {
        self.stat(path).await.is_ok()
    }
    async fn stat(&self, path: &Path) -> io::Result<FsStat>;
    /// Entries sorted by name.
    async fn read_dir(&self, path: &Path) -> io::Result<Vec<DirEntry>>;
    async fn mkdir(&self, path: &Path, recursive: bool) -> io::Result<()>;
    async fn remove(&self, path: &Path, recursive: bool) -> io::Result<()>;
    async fn rename(&self, from: &Path, to: &Path) -> io::Result<()>;
}

/// Recursive mkdir that tolerates an existing directory, for any [`Fs`].
pub async fn ensure_dir(fs: &dyn Fs, path: &Path) -> io::Result<()> {
    if fs.exists(path).await {
        return Ok(());
    }
    fs.mkdir(path, true).await
}

/// A NUL byte in the first 8000 bytes: binary, the way git and grep guess it.
pub fn looks_binary(buf: &[u8]) -> bool {
    buf.iter().take(8000).any(|&b| b == 0)
}

/// `/a/./b/../c` -> `/a/c`, without touching any file system.
pub fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::from("/");
    for c in path.components() {
        match c {
            Component::RootDir | Component::Prefix(_) | Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            Component::Normal(s) => out.push(s),
        }
    }
    out
}

fn not_found(path: &Path) -> io::Error {
    io::Error::new(
        io::ErrorKind::NotFound,
        format!("{}: no such file or directory", path.display()),
    )
}

#[derive(Debug, Clone)]
enum Node {
    File { data: Vec<u8>, modified: SystemTime },
    Dir,
}

/// An in-memory [`Fs`]: tests, scratch space, a sandbox with no disk behind it.
#[derive(Debug)]
pub struct MemFs {
    nodes: Mutex<BTreeMap<PathBuf, Node>>,
}

impl Default for MemFs {
    fn default() -> Self {
        let mut nodes = BTreeMap::new();
        nodes.insert(PathBuf::from("/"), Node::Dir);
        Self {
            nodes: Mutex::new(nodes),
        }
    }
}

impl MemFs {
    pub fn new() -> Self {
        Self::default()
    }

    /// Seed files (parents created), for tests.
    pub fn with_files<P: AsRef<Path>, D: AsRef<[u8]>>(
        files: impl IntoIterator<Item = (P, D)>,
    ) -> Self {
        let fs = Self::new();
        {
            let mut nodes = fs.nodes.lock().unwrap();
            for (p, d) in files {
                let p = normalize(p.as_ref());
                let mut parent = p.parent().map(Path::to_path_buf);
                while let Some(dir) = parent {
                    nodes.entry(dir.clone()).or_insert(Node::Dir);
                    parent = dir.parent().map(Path::to_path_buf);
                }
                nodes.insert(
                    p,
                    Node::File {
                        data: d.as_ref().to_vec(),
                        modified: SystemTime::now(),
                    },
                );
            }
        }
        fs
    }

    fn parent_is_dir(nodes: &BTreeMap<PathBuf, Node>, p: &Path) -> io::Result<()> {
        match p.parent() {
            None => Ok(()),
            Some(parent) => match nodes.get(parent) {
                Some(Node::Dir) => Ok(()),
                Some(Node::File { .. }) => Err(io::Error::new(
                    io::ErrorKind::NotADirectory,
                    parent.display().to_string(),
                )),
                None => Err(not_found(parent)),
            },
        }
    }

    fn children<'a>(
        nodes: &'a BTreeMap<PathBuf, Node>,
        dir: &'a Path,
    ) -> impl Iterator<Item = (&'a PathBuf, &'a Node)> {
        nodes
            .range(dir.to_path_buf()..)
            .skip(1)
            .take_while(move |(k, _)| k.starts_with(dir))
    }
}

#[async_trait]
impl Fs for MemFs {
    async fn read(&self, path: &Path) -> io::Result<Vec<u8>> {
        let p = normalize(path);
        match self.nodes.lock().unwrap().get(&p) {
            Some(Node::File { data, .. }) => Ok(data.clone()),
            Some(Node::Dir) => Err(io::Error::new(
                io::ErrorKind::IsADirectory,
                p.display().to_string(),
            )),
            None => Err(not_found(&p)),
        }
    }

    async fn write(&self, path: &Path, data: &[u8]) -> io::Result<()> {
        let p = normalize(path);
        let mut nodes = self.nodes.lock().unwrap();
        Self::parent_is_dir(&nodes, &p)?;
        if matches!(nodes.get(&p), Some(Node::Dir)) {
            return Err(io::Error::new(
                io::ErrorKind::IsADirectory,
                p.display().to_string(),
            ));
        }
        nodes.insert(
            p,
            Node::File {
                data: data.to_vec(),
                modified: SystemTime::now(),
            },
        );
        Ok(())
    }

    async fn append(&self, path: &Path, data: &[u8]) -> io::Result<()> {
        let p = normalize(path);
        let mut nodes = self.nodes.lock().unwrap();
        Self::parent_is_dir(&nodes, &p)?;
        match nodes.get_mut(&p) {
            Some(Node::File { data: d, modified }) => {
                d.extend_from_slice(data);
                *modified = SystemTime::now();
            }
            Some(Node::Dir) => {
                return Err(io::Error::new(
                    io::ErrorKind::IsADirectory,
                    p.display().to_string(),
                ));
            }
            None => {
                nodes.insert(
                    p,
                    Node::File {
                        data: data.to_vec(),
                        modified: SystemTime::now(),
                    },
                );
            }
        }
        Ok(())
    }

    async fn stat(&self, path: &Path) -> io::Result<FsStat> {
        let p = normalize(path);
        match self.nodes.lock().unwrap().get(&p) {
            Some(Node::File { data, modified }) => Ok(FsStat {
                kind: FileKind::File,
                size: data.len() as u64,
                modified: Some(*modified),
            }),
            Some(Node::Dir) => Ok(FsStat {
                kind: FileKind::Dir,
                size: 0,
                modified: None,
            }),
            None => Err(not_found(&p)),
        }
    }

    async fn read_dir(&self, path: &Path) -> io::Result<Vec<DirEntry>> {
        let p = normalize(path);
        let nodes = self.nodes.lock().unwrap();
        match nodes.get(&p) {
            Some(Node::Dir) => {}
            Some(Node::File { .. }) => {
                return Err(io::Error::new(
                    io::ErrorKind::NotADirectory,
                    p.display().to_string(),
                ));
            }
            None => return Err(not_found(&p)),
        }
        Ok(Self::children(&nodes, &p)
            .filter(|(k, _)| k.parent() == Some(p.as_path()))
            .map(|(k, n)| DirEntry {
                name: k
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into_owned(),
                kind: if matches!(n, Node::Dir) {
                    FileKind::Dir
                } else {
                    FileKind::File
                },
            })
            .collect())
    }

    async fn mkdir(&self, path: &Path, recursive: bool) -> io::Result<()> {
        let p = normalize(path);
        let mut nodes = self.nodes.lock().unwrap();
        match nodes.get(&p) {
            Some(Node::Dir) if recursive => return Ok(()),
            Some(_) => {
                return Err(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    p.display().to_string(),
                ));
            }
            None => {}
        }
        if recursive {
            for a in p.ancestors().collect::<Vec<_>>().into_iter().rev() {
                match nodes.get(a) {
                    Some(Node::File { .. }) => {
                        return Err(io::Error::new(
                            io::ErrorKind::NotADirectory,
                            a.display().to_string(),
                        ));
                    }
                    Some(Node::Dir) => {}
                    None => {
                        nodes.insert(a.to_path_buf(), Node::Dir);
                    }
                }
            }
            return Ok(());
        }
        Self::parent_is_dir(&nodes, &p)?;
        nodes.insert(p, Node::Dir);
        Ok(())
    }

    async fn remove(&self, path: &Path, recursive: bool) -> io::Result<()> {
        let p = normalize(path);
        let mut nodes = self.nodes.lock().unwrap();
        match nodes.get(&p) {
            None => return Err(not_found(&p)),
            Some(Node::Dir) => {
                let kids: Vec<PathBuf> =
                    Self::children(&nodes, &p).map(|(k, _)| k.clone()).collect();
                if !kids.is_empty() && !recursive {
                    return Err(io::Error::new(
                        io::ErrorKind::DirectoryNotEmpty,
                        p.display().to_string(),
                    ));
                }
                for k in kids {
                    nodes.remove(&k);
                }
            }
            Some(Node::File { .. }) => {}
        }
        nodes.remove(&p);
        Ok(())
    }

    async fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
        let (from, to) = (normalize(from), normalize(to));
        let mut nodes = self.nodes.lock().unwrap();
        if !nodes.contains_key(&from) {
            return Err(not_found(&from));
        }
        Self::parent_is_dir(&nodes, &to)?;
        let moved: Vec<PathBuf> = std::iter::once(from.clone())
            .chain(Self::children(&nodes, &from).map(|(k, _)| k.clone()))
            .collect();
        for old in moved {
            let node = nodes.remove(&old).expect("listed above");
            let rest = old.strip_prefix(&from).expect("under from");
            nodes.insert(to.join(rest), node);
        }
        Ok(())
    }
}

/// An [`Fs`] that refuses everything: a workspace with no files at all.
#[derive(Debug, Default)]
pub struct ShutFs;

fn shut() -> io::Error {
    io::Error::new(
        io::ErrorKind::PermissionDenied,
        "this workspace has no file system",
    )
}

#[async_trait]
impl Fs for ShutFs {
    async fn read(&self, _: &Path) -> io::Result<Vec<u8>> {
        Err(shut())
    }
    async fn write(&self, _: &Path, _: &[u8]) -> io::Result<()> {
        Err(shut())
    }
    async fn append(&self, _: &Path, _: &[u8]) -> io::Result<()> {
        Err(shut())
    }
    async fn stat(&self, _: &Path) -> io::Result<FsStat> {
        Err(shut())
    }
    async fn read_dir(&self, _: &Path) -> io::Result<Vec<DirEntry>> {
        Err(shut())
    }
    async fn mkdir(&self, _: &Path, _: bool) -> io::Result<()> {
        Err(shut())
    }
    async fn remove(&self, _: &Path, _: bool) -> io::Result<()> {
        Err(shut())
    }
    async fn rename(&self, _: &Path, _: &Path) -> io::Result<()> {
        Err(shut())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn memfs_reads_what_it_wrote_and_lists_direct_children_only() {
        let fs = MemFs::with_files([("/p/a.txt", "A"), ("/p/sub/b.txt", "B")]);
        assert_eq!(fs.read_to_string(Path::new("/p/a.txt")).await.unwrap(), "A");
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
        let fs = MemFs::with_files([("/d/x/1", "1")]);
        fs.rename(Path::new("/d/x"), Path::new("/d/y"))
            .await
            .unwrap();
        assert!(fs.exists(Path::new("/d/y/1")).await);
        assert!(!fs.exists(Path::new("/d/x")).await);
        assert!(fs.remove(Path::new("/d"), false).await.is_err());
        fs.remove(Path::new("/d"), true).await.unwrap();
        assert!(!fs.exists(Path::new("/d/y/1")).await);
    }

    #[test]
    fn normalize_resolves_dots_lexically() {
        assert_eq!(normalize(Path::new("/a/./b/../c")), PathBuf::from("/a/c"));
        assert_eq!(normalize(Path::new("/../x")), PathBuf::from("/x"));
    }
}
