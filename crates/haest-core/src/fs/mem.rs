//! An in-memory [`Fs`]: tests, scratch space, a sandbox with no disk behind it.

use std::{
    collections::BTreeMap,
    io::{self, ErrorKind},
    path::{Path, PathBuf},
    sync::Mutex,
    time::SystemTime,
};

use async_trait::async_trait;

use super::{DirEntry, FileKind, Fs, FsStat, normalize, not_found};

#[derive(Debug, Clone)]
enum Node {
    File { data: Vec<u8>, modified: SystemTime },
    Dir,
}

impl Node {
    fn file(data: &[u8]) -> Self {
        Node::File {
            data: data.to_vec(),
            modified: SystemTime::now(),
        }
    }
}

/// `kind`, naming `p`.
fn err(kind: ErrorKind, p: &Path) -> io::Error {
    io::Error::new(kind, p.display().to_string())
}

/// An in-memory [`Fs`]: tests, scratch space, a sandbox with no disk behind it.
///
/// Files and directories only: no links ([`Fs::read_link`] says no path is
/// one) and no permissions ([`Fs::chmod`] does nothing). Paths are
/// [`normalize`]d on the way in. It starts with `/` alone, and `/` cannot be
/// removed. Safe to share between tasks; every call takes one short lock.
///
/// # Examples
///
/// ```
/// use std::path::Path;
///
/// use haest_core::{Fs, MemFs};
///
/// # #[tokio::main(flavor = "current_thread")]
/// # async fn main() -> std::io::Result<()> {
/// let fs = MemFs::with_files([("/p/a.txt", "A"), ("/p/sub/b.txt", "B")]);
/// fs.rename(Path::new("/p/sub"), Path::new("/p/moved")).await?;
/// assert_eq!(fs.read_to_string(Path::new("/p/moved/b.txt")).await?, "B");
/// assert!(fs.remove(Path::new("/p"), false).await.is_err(), "not empty");
/// fs.remove(Path::new("/p"), true).await?;
/// assert!(!fs.exists(Path::new("/p/a.txt")).await);
/// # Ok(())
/// # }
/// ```
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
    /// An empty file system: only the root directory `/`.
    pub fn new() -> Self {
        Self::default()
    }

    /// A file system holding these `(path, contents)` files, every parent
    /// directory created. A path given twice keeps the last contents.
    pub fn with_files<P: AsRef<Path>, D: AsRef<[u8]>>(
        files: impl IntoIterator<Item = (P, D)>,
    ) -> Self {
        let fs = Self::new();
        {
            let mut nodes = fs.nodes.lock().unwrap();
            for (p, d) in files {
                let p = normalize(p.as_ref());
                for dir in p.ancestors().skip(1) {
                    nodes.entry(dir.to_path_buf()).or_insert(Node::Dir);
                }
                nodes.insert(p, Node::file(d.as_ref()));
            }
        }
        fs
    }

    fn parent_is_dir(nodes: &BTreeMap<PathBuf, Node>, p: &Path) -> io::Result<()> {
        match p.parent() {
            None => Ok(()),
            Some(parent) => match nodes.get(parent) {
                Some(Node::Dir) => Ok(()),
                Some(Node::File { .. }) => Err(err(ErrorKind::NotADirectory, parent)),
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

    /// The file's bytes, up to `max` of them, copied out from under the lock.
    fn copy(&self, path: &Path, max: usize) -> io::Result<Vec<u8>> {
        let p = normalize(path);
        match self.nodes.lock().unwrap().get(&p) {
            Some(Node::File { data, .. }) => Ok(data[..max.min(data.len())].to_vec()),
            Some(Node::Dir) => Err(err(ErrorKind::IsADirectory, &p)),
            None => Err(not_found(&p)),
        }
    }
}

#[async_trait]
impl Fs for MemFs {
    async fn read(&self, path: &Path) -> io::Result<Vec<u8>> {
        self.copy(path, usize::MAX)
    }

    async fn read_head(&self, path: &Path, max: usize) -> io::Result<Vec<u8>> {
        self.copy(path, max)
    }

    async fn write(&self, path: &Path, data: &[u8]) -> io::Result<()> {
        let p = normalize(path);
        let mut nodes = self.nodes.lock().unwrap();
        Self::parent_is_dir(&nodes, &p)?;
        if matches!(nodes.get(&p), Some(Node::Dir)) {
            return Err(err(ErrorKind::IsADirectory, &p));
        }
        nodes.insert(p, Node::file(data));
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
            Some(Node::Dir) => return Err(err(ErrorKind::IsADirectory, &p)),
            None => {
                nodes.insert(p, Node::file(data));
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
            Some(Node::File { .. }) => return Err(err(ErrorKind::NotADirectory, &p)),
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
            Some(_) => return Err(err(ErrorKind::AlreadyExists, &p)),
            None => {}
        }
        if !recursive {
            Self::parent_is_dir(&nodes, &p)?;
            nodes.insert(p, Node::Dir);
            return Ok(());
        }
        for a in p.ancestors().collect::<Vec<_>>().into_iter().rev() {
            match nodes.get(a) {
                Some(Node::File { .. }) => return Err(err(ErrorKind::NotADirectory, a)),
                Some(Node::Dir) => {}
                None => {
                    nodes.insert(a.to_path_buf(), Node::Dir);
                }
            }
        }
        Ok(())
    }

    async fn remove(&self, path: &Path, recursive: bool) -> io::Result<()> {
        let p = normalize(path);
        // EBUSY, as rmdir(2) on `/`: without the root every later write is NotFound.
        if p.parent().is_none() {
            return Err(err(ErrorKind::ResourceBusy, &p));
        }
        let mut nodes = self.nodes.lock().unwrap();
        match nodes.get(&p) {
            None => return Err(not_found(&p)),
            Some(Node::Dir) => {
                let kids: Vec<PathBuf> =
                    Self::children(&nodes, &p).map(|(k, _)| k.clone()).collect();
                if !kids.is_empty() && !recursive {
                    return Err(err(ErrorKind::DirectoryNotEmpty, &p));
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
        // EINVAL, as rename(2): moved into itself it would be cut off from the tree.
        if to != from && to.starts_with(&from) {
            return Err(err(ErrorKind::InvalidInput, &to));
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
