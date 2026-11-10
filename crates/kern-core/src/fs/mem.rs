//! An in-memory [`Fs`]: tests, scratch space, a sandbox with no disk behind it.

use std::{
    collections::BTreeMap,
    io,
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

    /// The file's bytes, up to `max` of them, copied out from under the lock.
    fn copy(&self, path: &Path, max: usize) -> io::Result<Vec<u8>> {
        let p = normalize(path);
        match self.nodes.lock().unwrap().get(&p) {
            Some(Node::File { data, .. }) => Ok(data[..max.min(data.len())].to_vec()),
            Some(Node::Dir) => Err(io::Error::new(
                io::ErrorKind::IsADirectory,
                p.display().to_string(),
            )),
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
