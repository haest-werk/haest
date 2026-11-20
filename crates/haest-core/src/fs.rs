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
//! use haest_core::{
//!     Fs, MemFs,
//!     fs::{ensure_dir, read_text_if_present},
//! };
//!
//! # #[tokio::main(flavor = "current_thread")]
//! # async fn main() -> std::io::Result<()> {
//! let fs = MemFs::from_files([("/project/README.md", "# Hello")]);
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
    any::Any,
    fmt, io,
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
///
/// Built with [`FsStat::new`] and [`with_modified`](FsStat::with_modified),
/// so a field added later (a mode) breaks no [`Fs`] outside haest.
///
/// ```
/// use std::time::SystemTime;
///
/// use haest_core::fs::{FileKind, FsStat};
///
/// let stat = FsStat::new(FileKind::File, 42).with_modified(SystemTime::UNIX_EPOCH);
/// assert!(stat.is_file());
/// assert_eq!((stat.size, stat.modified), (42, Some(SystemTime::UNIX_EPOCH)));
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct FsStat {
    /// File, directory or link.
    pub kind: FileKind,
    /// Size in bytes (0 for a directory in [`MemFs`]; whatever the platform says on disk).
    pub size: u64,
    /// When it last changed; None where the file system keeps no time.
    pub modified: Option<SystemTime>,
}

impl FsStat {
    /// A path of `kind` holding `size` bytes, with no time it last changed.
    pub fn new(kind: FileKind, size: u64) -> Self {
        Self {
            kind,
            size,
            modified: None,
        }
    }

    /// The same, last changed `at`.
    #[must_use]
    pub fn with_modified(mut self, at: SystemTime) -> Self {
        self.modified = Some(at);
        self
    }

    /// Is it a directory?
    pub fn is_dir(&self) -> bool {
        self.kind == FileKind::Dir
    }
    /// Is it a file?
    pub fn is_file(&self) -> bool {
        self.kind == FileKind::File
    }
}

/// One entry of a directory listing ([`Fs::read_dir`]), built with
/// [`DirEntry::new`].
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct DirEntry {
    /// The entry's own name, no directory part.
    pub name: String,
    /// What it is; a link is [`FileKind::Symlink`], not followed.
    pub kind: FileKind,
}

impl DirEntry {
    /// The entry `name` (no directory part), of `kind`.
    pub fn new(name: impl Into<String>, kind: FileKind) -> Self {
        Self {
            name: name.into(),
            kind,
        }
    }
}

/// A lock on a file, held until this value is dropped: what [`Fs::try_lock`]
/// hands back.
///
/// It holds whatever releases the lock as it drops: the open file the
/// system's lock is on, or a guard of the host's own store.
///
/// ```
/// use std::sync::{
///     Arc,
///     atomic::{AtomicBool, Ordering},
/// };
///
/// use haest_core::fs::FsLock;
///
/// /// A host's store marks a file held; this unmarks it as it drops.
/// struct Unmark(Arc<AtomicBool>);
///
/// impl Drop for Unmark {
///     fn drop(&mut self) {
///         self.0.store(false, Ordering::SeqCst);
///     }
/// }
///
/// let held = Arc::new(AtomicBool::new(true));
/// let lock = FsLock::new(Unmark(held.clone()));
/// assert!(held.load(Ordering::SeqCst));
/// drop(lock);
/// assert!(!held.load(Ordering::SeqCst));
/// ```
pub struct FsLock {
    _held: Box<dyn Any + Send + Sync>,
}

impl FsLock {
    /// A lock held until this value is dropped, which drops `held`: what
    /// releases the lock as it drops.
    pub fn new(held: impl Send + Sync + 'static) -> Self {
        Self {
            _held: Box::new(held),
        }
    }
}

/// Nothing of what it holds.
impl fmt::Debug for FsLock {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FsLock").finish_non_exhaustive()
    }
}

/// A file system: the one way haest reads and writes files.
///
/// Every path is absolute (see [`resolve_path`]). Errors are `io::Error`s with
/// the [`io::ErrorKind`] `std::fs` would give for the same failure: callers
/// match on them ([`read_text_if_present`] treats `NotFound` and
/// `NotADirectory` as "no file"; the sandboxed shell turns each kind into its
/// own error).
///
/// Nine methods are required: `read`, `write`, `append`, `stat`, `read_dir`,
/// `create_dir`, `remove`, `remove_all` (a store with no one-step removal
/// walks the tree with [`remove_tree`]) and `rename`. The rest have defaults
/// built on them; override one when the store can do it better
/// ([`Fs::read_head`] that stops early, [`Fs::create_dir_all`] in one step,
/// [`Fs::write_whole`] on a disk whose write empties a file first,
/// [`Fs::read_link`] and [`Fs::chmod`] on a store that has links and modes,
/// [`Fs::try_lock`] on a store more than one writer shares).
/// A host's wrapper over another `Fs` forwards each method it does not
/// change, [`Fs::write_whole`] and [`Fs::try_lock`] above all (left to its
/// default, the one writes with the inner store's plain `write`, which on
/// the real disk empties the file first, and the other holds nothing), and
/// puts `#[warn(clippy::missing_trait_methods)]` on its impl, which names
/// each one it left out.
/// [`FsStat`] and [`DirEntry`] are built with their `new`.
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
/// use haest_core::{DirEntry, Fs, FsStat, MemFs};
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
///     async fn create_dir(&self, _: &Path) -> io::Result<()> {
///         Err(refused())
///     }
///     async fn remove(&self, _: &Path) -> io::Result<()> {
///         Err(refused())
///     }
///     async fn remove_all(&self, _: &Path) -> io::Result<()> {
///         Err(refused())
///     }
///     async fn rename(&self, _: &Path, _: &Path) -> io::Result<()> {
///         Err(refused())
///     }
/// }
///
/// # #[tokio::main(flavor = "current_thread")]
/// # async fn main() -> io::Result<()> {
/// let fs = ReadOnly(MemFs::from_files([("/docs/a.md", "A")]));
/// assert_eq!(fs.read_to_string(Path::new("/docs/a.md")).await?, "A");
/// assert!(fs.exists(Path::new("/docs")).await);
/// let refused = fs.write(Path::new("/docs/b.md"), b"B").await;
/// assert!(refused.is_err_and(|e| e.kind() == io::ErrorKind::PermissionDenied));
/// # Ok(())
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
    /// Replace what the file at `path` holds with `data`, whole or not at
    /// all where the store can: a write it stops partway (a full disk, a
    /// kill) leaves the file as it was, never emptied or cut short. The
    /// tools that rewrite a file that is there call it (Edit, Write over
    /// one, `GenerateImage` over an image), since the model cannot put back
    /// a file it only edited, and so does the overlay's apply; a new file
    /// is made with [`Fs::write`].
    ///
    /// The default is [`Fs::write`]: a store whose write cannot stop
    /// partway (memory), or that keeps writes its own way (a host's,
    /// staging them for review), has nothing to add. A disk that empties a
    /// file before it writes it overrides it (the real disk writes a new
    /// file beside it and renames it over); a wrapper over another `Fs`
    /// forwards it.
    ///
    /// # Errors
    ///
    /// [`Fs::write`]'s, and the store's own where it cannot replace the
    /// file whole (no room for the new copy), with the file as it was. A
    /// store that writes some files in place all the same (the real disk:
    /// one with another hard link, a symlink) can fail with such a file
    /// cut short.
    async fn write_whole(&self, path: &Path, data: &[u8]) -> io::Result<()> {
        self.write(path, data).await
    }
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
    /// Create the directory `path`; its parent must exist.
    ///
    /// # Errors
    ///
    /// `AlreadyExists` when something is at `path`, `NotFound` when the
    /// parent is missing, and an error when a file sits where the parent
    /// should be.
    async fn create_dir(&self, path: &Path) -> io::Result<()>;
    /// Create the directory `path` and every missing parent, like `mkdir -p`;
    /// a directory already there is fine. The default walks down from the
    /// root with [`Fs::stat`] and [`Fs::create_dir`]; a store that can do it
    /// in one step should.
    ///
    /// # Errors
    ///
    /// `NotADirectory` when a file sits where a directory should be, and
    /// [`Fs::create_dir`]'s.
    async fn create_dir_all(&self, path: &Path) -> io::Result<()> {
        let dirs: Vec<&Path> = path.ancestors().collect();
        for dir in dirs.into_iter().rev() {
            match self.stat(dir).await {
                Ok(stat) if stat.is_dir() => {}
                Ok(_) => {
                    return Err(io::Error::new(
                        io::ErrorKind::NotADirectory,
                        format!("{}: not a directory", dir.display()),
                    ));
                }
                Err(_) => match self.create_dir(dir).await {
                    Ok(()) => {}
                    // Another writer made it first: fine, if it is a directory.
                    Err(e)
                        if e.kind() == io::ErrorKind::AlreadyExists
                            && self.stat(dir).await.is_ok_and(|s| s.is_dir()) => {}
                    Err(e) => return Err(e),
                },
            }
        }
        Ok(())
    }
    /// Delete a file, a link (not what it names) or an empty directory.
    ///
    /// # Errors
    ///
    /// `NotFound` when nothing is there, and an error for a directory with
    /// anything in it ([`Fs::remove_all`] deletes those).
    async fn remove(&self, path: &Path) -> io::Result<()>;
    /// Delete `path` and, for a directory, everything in it. A link is
    /// deleted, never followed.
    ///
    /// Required: deleting a whole tree is where a store's own rules belong
    /// (a cap on how much one call removes, one step instead of many). A
    /// store with nothing of its own to say calls [`remove_tree`], which
    /// walks the tree with [`Fs::read_link`], [`Fs::read_dir`] and
    /// [`Fs::remove`].
    ///
    /// # Errors
    ///
    /// `NotFound` when nothing is there, and an error when any part of the
    /// tree could not be removed.
    async fn remove_all(&self, path: &Path) -> io::Result<()>;
    /// Move a file or a whole directory tree from `from` to `to`, as
    /// rename(2): a file replaces a file at `to`, a directory an empty
    /// directory, and onto itself nothing changes. The parent of `to` must
    /// exist.
    ///
    /// # Errors
    ///
    /// `NotFound` when `from` (or `to`'s parent) is missing; `InvalidInput`
    /// when `to` is inside the directory `from`; `IsADirectory` for a file
    /// onto a directory, `NotADirectory` for a directory onto a file, and
    /// `DirectoryNotEmpty` for a directory onto one with anything in it.
    async fn rename(&self, from: &Path, to: &Path) -> io::Result<()>;
    /// Set Unix permission bits (the session log is 0600 in a 0700 dir). The
    /// default does nothing: a file system with no permissions (memory, a
    /// host's own store) has nothing to set.
    async fn chmod(&self, _path: &Path, _mode: u32) -> io::Result<()> {
        Ok(())
    }
    /// Hold the file at `path` for one writer until the returned lock is
    /// dropped; it never waits for one. Meanwhile every other `try_lock` of
    /// the file, in this process or another, fails. A lock must not outlive
    /// the process that holds it, so a crash never leaves a file locked. A
    /// store makes the file, empty, when it is not there (its dir must be).
    /// A session is held this way while a conversation writes it, through a
    /// lock file of its own beside the session file
    /// ([`JsonlSessionFile::try_lock`](crate::session::JsonlSessionFile::try_lock)).
    ///
    /// `Ok(None)` when the file system has no locks: nothing is held, nothing
    /// is refused, and no file is made. That is the default (memory, a
    /// host's own store that one writer uses); a store more than one writer
    /// shares overrides it.
    ///
    /// # Errors
    ///
    /// `WouldBlock` when another holder has the file, and the file system's
    /// other errors: the file cannot be made or opened, or the lock cannot be
    /// taken.
    async fn try_lock(&self, _path: &Path) -> io::Result<Option<FsLock>> {
        Ok(None)
    }
}

crate::plug_ref! {
    /// An [`Fs`], shared: what [`Workspace::fs`](crate::Workspace::fs) hands
    /// out, and what a session file or a skill set reads through. Built from
    /// the file system itself (`MemFs::new()`, a host's own), or from an
    /// `Arc` of one.
    pub struct FsRef(dyn Fs);
}

mod mem;
mod ops;
mod paths;
mod shut;
pub use mem::MemFs;
pub use ops::{ensure_dir, looks_binary, read_text_if_present, remove_tree};
pub(crate) use ops::{not_found, read_lossy};
pub use paths::{normalize, resolve_path};
pub use shut::ShutFs;

#[cfg(test)]
mod tests;
