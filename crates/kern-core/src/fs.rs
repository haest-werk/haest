//! The file system the agent sees. Every file operation kern performs -- tools,
//! spilled output, the session log, instruction files -- goes through an [`Fs`].
//! Nothing in kern touches `std::fs` for the agent directly.
//!
//! PLUG POINT: a host with its own storage (brain's `BrainFs`) implements [`Fs`];
//! `kern-workspace` can put a sandboxed bash over ANY `Fs`, so the file the agent
//! writes with Write is the file `cat` shows.
//!
//! Paths are absolute. A relative path is the caller's bug ([`resolve_path`]).

use std::{
    io,
    path::{Component, Path, PathBuf},
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
    /// The first `max` bytes (all of a shorter file), for a caller that needs
    /// only the start of many files (a list of saved sessions). The default
    /// reads the whole file, then cuts it; a file system that can stop early
    /// should.
    async fn read_head(&self, path: &Path, max: usize) -> io::Result<Vec<u8>> {
        let mut bytes = self.read(path).await?;
        bytes.truncate(max);
        Ok(bytes)
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
    /// Set Unix permission bits (the session log is 0600 in a 0700 dir). The
    /// default does nothing: a file system with no permissions (memory, a
    /// host's own store) has nothing to set.
    async fn chmod(&self, _path: &Path, _mode: u32) -> io::Result<()> {
        Ok(())
    }
}

/// Recursive mkdir that tolerates an existing directory, for any [`Fs`].
pub async fn ensure_dir(fs: &dyn Fs, path: &Path) -> io::Result<()> {
    if fs.exists(path).await {
        return Ok(());
    }
    fs.mkdir(path, true).await
}

/// A file a host may or may not have (a config file, `.mcp.json`): its text,
/// None when there is no such file -- nothing at the path, or a file where one
/// of its dirs should be. One read: no `exists` first. A file that is there
/// but cannot be read is an error, never a silent default. Strict UTF-8, like
/// [`Fs::read_to_string`].
pub async fn read_text_if_present(fs: &dyn Fs, path: &Path) -> io::Result<Option<String>> {
    match fs.read_to_string(path).await {
        Ok(text) => Ok(Some(text)),
        Err(e)
            if matches!(
                e.kind(),
                io::ErrorKind::NotFound | io::ErrorKind::NotADirectory
            ) =>
        {
            Ok(None)
        }
        Err(e) => Err(e),
    }
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

/// Absolute paths stay; relative ones are taken from `cwd`. Dots are resolved
/// lexically ([`normalize`]).
pub fn resolve_path(cwd: &Path, p: &str) -> PathBuf {
    let path = Path::new(p);
    if path.is_absolute() {
        normalize(path)
    } else {
        normalize(&cwd.join(path))
    }
}

pub(crate) fn not_found(path: &Path) -> io::Error {
    io::Error::new(
        io::ErrorKind::NotFound,
        format!("{}: no such file or directory", path.display()),
    )
}

mod mem;
mod shut;
pub use mem::MemFs;
pub use shut::ShutFs;

#[cfg(test)]
mod tests;
