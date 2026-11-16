//! The file system the agent sees. Every file operation haest performs -- tools,
//! spilled output, the session log, instruction files, skills -- goes through
//! an [`Fs`]. Nothing in haest touches `std::fs` for the agent directly.
//!
//! PLUG POINT: a host with its own storage implements [`Fs`] (the `haest`
//! crate's `notebook` example stages every write for review); `haest-workspace`
//! has the real disk (`RealFs`) and can put a sandboxed bash over ANY `Fs`, so
//! the file the agent writes with Write is the file `cat` shows. A
//! [`Workspace`](crate::Workspace) hands its `Fs` to the tools.
//!
//! Here: the trait, [`MemFs`] (in memory: tests, scratch, a sandbox with no
//! disk), [`ShutFs`] (refuses everything), and the path helpers every
//! implementation and tool shares.
//!
//! Paths are absolute. A relative path is the caller's bug: resolve it against
//! the cwd first ([`resolve_path`]).
//!
//! # Examples
//!
//! ```
//! use std::path::Path;
//!
//! use haest_core::fs::{Fs, MemFs, ensure_dir, read_text_if_present};
//!
//! # #[tokio::main(flavor = "current_thread")]
//! # async fn main() -> std::io::Result<()> {
//! let fs = MemFs::with_files([("/project/README.md", "# Hello")]);
//! assert_eq!(fs.read_to_string(Path::new("/project/README.md")).await?, "# Hello");
//!
//! ensure_dir(&fs, Path::new("/project/notes")).await?;
//! fs.write(Path::new("/project/notes/a.md"), b"first").await?;
//! fs.append(Path::new("/project/notes/a.md"), b", second").await?;
//! assert_eq!(fs.read(Path::new("/project/notes/a.md")).await?, b"first, second");
//!
//! let names: Vec<String> = fs.read_dir(Path::new("/project")).await?
//!     .into_iter().map(|e| e.name).collect();
//! assert_eq!(names, ["README.md", "notes"]);
//!
//! // A file a host may or may not have: None, not an error.
//! assert_eq!(read_text_if_present(&fs, Path::new("/project/.mcp.json")).await?, None);
//! # Ok(())
//! # }
//! ```

use std::{
    io,
    path::{Path, PathBuf},
    time::SystemTime,
};

use async_trait::async_trait;

/// What a path is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileKind {
    /// A file (anything that is neither a directory nor a link).
    File,
    /// A directory.
    Dir,
    /// A symbolic link, as [`Fs::read_dir`] reports one; [`Fs::stat`] follows
    /// links, so it reports what the link names instead.
    Symlink,
}

/// What [`Fs::stat`] knows about a path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FsStat {
    /// File, directory or link.
    pub kind: FileKind,
    /// Size in bytes (0 for a directory in [`MemFs`]; whatever the platform says on disk).
    pub size: u64,
    /// When it last changed; None where the file system keeps no time.
    pub modified: Option<SystemTime>,
}

impl FsStat {
    /// Is it a directory?
    pub fn is_dir(&self) -> bool {
        self.kind == FileKind::Dir
    }
    /// Is it a file?
    pub fn is_file(&self) -> bool {
        self.kind == FileKind::File
    }
}

/// One entry of a directory listing ([`Fs::read_dir`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirEntry {
    /// The entry's own name, no directory part.
    pub name: String,
    /// What it is; a link is [`FileKind::Symlink`], not followed.
    pub kind: FileKind,
}

/// A file system: the one way haest reads and writes files.
///
/// Every path is absolute (see [`resolve_path`]). Errors are `io::Error`s with
/// the [`io::ErrorKind`] `std::fs` would give for the same failure: callers
/// match on them ([`read_text_if_present`] treats `NotFound` and
/// `NotADirectory` as "no file"; the sandboxed shell turns each kind into its
/// own error).
///
/// Eight methods are required. The rest have defaults built on them; override
/// one when the store can do it better ([`Fs::read_head`] that stops early,
/// [`Fs::read_link`] and [`Fs::chmod`] on a store that has links and modes).
///
/// Implementations here: [`MemFs`] and [`ShutFs`]; `haest-workspace` has the
/// real disk (`RealFs`) and a copy-on-write sandbox over any `Fs`.
///
/// # Examples
///
/// A host's own store, here a read-only view over a [`MemFs`]:
///
/// ```
/// use std::{io, path::Path};
///
/// use async_trait::async_trait;
/// use haest_core::fs::{DirEntry, Fs, FsStat, MemFs};
///
/// struct ReadOnly(MemFs);
///
/// fn refused() -> io::Error {
///     io::Error::new(io::ErrorKind::PermissionDenied, "read-only")
/// }
///
/// #[async_trait]
/// impl Fs for ReadOnly {
///     async fn read(&self, path: &Path) -> io::Result<Vec<u8>> {
///         self.0.read(path).await
///     }
///     async fn stat(&self, path: &Path) -> io::Result<FsStat> {
///         self.0.stat(path).await
///     }
///     async fn read_dir(&self, path: &Path) -> io::Result<Vec<DirEntry>> {
///         self.0.read_dir(path).await
///     }
///     async fn write(&self, _: &Path, _: &[u8]) -> io::Result<()> {
///         Err(refused())
///     }
///     async fn append(&self, _: &Path, _: &[u8]) -> io::Result<()> {
///         Err(refused())
///     }
///     async fn mkdir(&self, _: &Path, _: bool) -> io::Result<()> {
///         Err(refused())
///     }
///     async fn remove(&self, _: &Path, _: bool) -> io::Result<()> {
///         Err(refused())
///     }
///     async fn rename(&self, _: &Path, _: &Path) -> io::Result<()> {
///         Err(refused())
///     }
/// }
///
/// # #[tokio::main(flavor = "current_thread")]
/// # async fn main() {
/// let fs = ReadOnly(MemFs::with_files([("/docs/a.md", "A")]));
/// assert_eq!(fs.read_to_string(Path::new("/docs/a.md")).await.unwrap(), "A");
/// assert!(fs.exists(Path::new("/docs")).await);
/// let err = fs.write(Path::new("/docs/b.md"), b"B").await.unwrap_err();
/// assert_eq!(err.kind(), io::ErrorKind::PermissionDenied);
/// # }
/// ```
#[async_trait]
pub trait Fs: Send + Sync {
    /// The whole file's bytes.
    ///
    /// # Errors
    ///
    /// `NotFound` when nothing is there, and an error for a directory. An
    /// implementation may refuse what is not a regular file: the real disk
    /// refuses a FIFO or a device (`InvalidInput`) rather than block on it.
    async fn read(&self, path: &Path) -> io::Result<Vec<u8>>;
    /// The whole file as text.
    ///
    /// # Errors
    ///
    /// [`Fs::read`]'s, and `InvalidData` when the bytes are not strict UTF-8.
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
    /// Create the file or replace its contents with `data`.
    ///
    /// # Errors
    ///
    /// `NotFound` when the parent directory does not exist (it is never
    /// created: see [`ensure_dir`]), and an error when `path` is a directory.
    async fn write(&self, path: &Path, data: &[u8]) -> io::Result<()>;
    /// Add `data` to the end of the file, creating it when it is missing.
    /// The session log writes every record this way.
    ///
    /// # Errors
    ///
    /// As [`Fs::write`]: the parent directory must exist, and `path` must not
    /// be a directory.
    async fn append(&self, path: &Path, data: &[u8]) -> io::Result<()>;
    /// Is anything at `path`? Any error from [`Fs::stat`] counts as no.
    async fn exists(&self, path: &Path) -> bool {
        self.stat(path).await.is_ok()
    }
    /// What is at `path`, following symbolic links: what a read or a write
    /// cares about is the file a link names.
    ///
    /// # Errors
    ///
    /// `NotFound` when nothing is there (a dangling link included).
    async fn stat(&self, path: &Path) -> io::Result<FsStat>;
    /// The target of the symbolic link at `path`, the link itself not
    /// followed; `InvalidInput` when `path` is not a link. The default is a
    /// file system with no links (memory, a host's own store): every path
    /// that exists is not one.
    ///
    /// # Errors
    ///
    /// `NotFound` when nothing is there, `InvalidInput` when it is not a link.
    async fn read_link(&self, path: &Path) -> io::Result<PathBuf> {
        self.stat(path).await?;
        Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{}: not a symbolic link", path.display()),
        ))
    }
    /// The directory's direct children, sorted by name. A link is listed as
    /// [`FileKind::Symlink`] and not followed, so a walk never descends a
    /// link loop.
    ///
    /// # Errors
    ///
    /// `NotFound` when nothing is there, `NotADirectory` for a file.
    async fn read_dir(&self, path: &Path) -> io::Result<Vec<DirEntry>>;
    /// Create a directory. `recursive` creates every missing parent too and
    /// accepts a directory that is already there, like `mkdir -p`.
    ///
    /// # Errors
    ///
    /// Without `recursive`: `AlreadyExists` when something is at `path`, and
    /// `NotFound` when the parent is missing. Either way, an error when a file
    /// sits where a directory should be.
    async fn mkdir(&self, path: &Path, recursive: bool) -> io::Result<()>;
    /// Delete a file, a link (not what it names) or a directory. A directory
    /// with anything in it needs `recursive`, which deletes it all.
    ///
    /// # Errors
    ///
    /// `NotFound` when nothing is there, and an error for a non-empty
    /// directory without `recursive`.
    async fn remove(&self, path: &Path, recursive: bool) -> io::Result<()>;
    /// Move a file or a whole directory tree from `from` to `to`, replacing a
    /// file at `to`. The parent of `to` must exist.
    ///
    /// # Errors
    ///
    /// `NotFound` when `from` (or `to`'s parent) is missing; `InvalidInput`
    /// when `to` is inside the directory `from`.
    async fn rename(&self, from: &Path, to: &Path) -> io::Result<()>;
    /// Set Unix permission bits (the session log is 0600 in a 0700 dir). The
    /// default does nothing: a file system with no permissions (memory, a
    /// host's own store) has nothing to set.
    async fn chmod(&self, _path: &Path, _mode: u32) -> io::Result<()> {
        Ok(())
    }
}

mod mem;
mod ops;
mod paths;
mod shut;
pub use mem::MemFs;
pub use ops::{ensure_dir, looks_binary, read_text_if_present};
pub(crate) use ops::{not_found, read_lossy};
pub use paths::{normalize, resolve_path};
pub use shut::ShutFs;

#[cfg(test)]
mod tests;
