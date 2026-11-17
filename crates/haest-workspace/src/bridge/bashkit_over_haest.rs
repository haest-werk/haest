//! Any haest [`Fs`](haest_core::fs::Fs) as a bashkit file system: a host's own storage gets a
//! sandboxed bash, and `cat` shows exactly what the Write tool wrote.
//!
//! What a haest `Fs` cannot hold, stated: no symlinks (`ln -s` fails), no
//! permission bits (a `chmod` is remembered here, in memory, so
//! `chmod +x f && ./f` works), no settable times (`touch` on an existing file
//! succeeds and leaves the time to the `Fs`).

use std::{
    collections::HashMap,
    io::{self, ErrorKind},
    path::{Path, PathBuf},
    sync::{Mutex, MutexGuard},
    time::SystemTime,
};

use async_trait::async_trait;
use bashkit::{DirEntry, FileSystem, FileSystemExt, FileType, Metadata};
use haest_core::fs::{FileKind, FsRef, FsStat, normalize};

use super::bashkit_from_io;

type Result<T> = bashkit::Result<T>;

/// Any haest [`Fs`](haest_core::fs::Fs) as a bashkit `FileSystem`: what puts a sandboxed bash over
/// a host's own storage ([`SandboxWorkspace::over`](crate::SandboxWorkspace::over)
/// builds one).
///
/// Every path is normalized (`/a/./b/../c` is `/a/c`) before it reaches the
/// `Fs`. An error the `Fs` returns reads like a shell's (`cat: f: No such file
/// or directory`), except a permission refusal with a message of the host's
/// own, which keeps it: the model reads why.
///
/// What a haest `Fs` cannot hold, stated:
/// - no symlinks: `ln -s` fails;
/// - no permission bits: a `chmod` is remembered here, in memory, for the life
///   of this object, so `chmod +x f && ./f` works;
/// - no settable times: `touch` on an existing file succeeds and leaves the
///   time to the `Fs`.
pub(crate) struct BashkitFsOverHaest {
    fs: FsRef,
    modes: Mutex<HashMap<PathBuf, u32>>,
}

impl BashkitFsOverHaest {
    /// Wrap `fs` so a bashkit shell can run over it.
    pub(crate) fn new(fs: FsRef) -> Self {
        Self {
            fs,
            modes: Mutex::default(),
        }
    }

    /// The remembered modes. The lock is held only to read or write the map,
    /// never across an await or a host's code, so a poisoned one is a bug here.
    fn modes(&self) -> MutexGuard<'_, HashMap<PathBuf, u32>> {
        self.modes
            .lock()
            .expect("the remembered modes' lock poisoned")
    }

    fn metadata(&self, path: &Path, st: &FsStat) -> Metadata {
        let (file_type, default_mode) = match st.kind {
            FileKind::File => (FileType::File, 0o644),
            FileKind::Dir => (FileType::Directory, 0o755),
            FileKind::Symlink => (FileType::Symlink, 0o777),
        };
        let mode = self.modes().get(path).copied();
        let modified = st.modified.unwrap_or_else(SystemTime::now);
        Metadata {
            file_type,
            size: st.size,
            mode: mode.unwrap_or(default_mode),
            modified,
            created: modified,
        }
    }

    async fn stat_path(&self, path: &Path) -> Result<FsStat> {
        self.fs.stat(path).await.map_err(bashkit_from_io)
    }

    /// Drop (or move) the remembered modes of `path` and everything under it.
    fn forget_modes(&self, path: &Path, moved_to: Option<&Path>) {
        let mut modes = self.modes();
        let under: Vec<PathBuf> = modes
            .keys()
            .filter(|k| k.starts_with(path))
            .cloned()
            .collect();
        for old in under {
            let mode = modes.remove(&old).expect("listed above");
            if let Some(to) = moved_to {
                let rest = old.strip_prefix(path).expect("under path");
                modes.insert(to.join(rest), mode);
            }
        }
    }
}

fn err(kind: ErrorKind, msg: &str) -> bashkit::Error {
    bashkit_from_io(io::Error::new(kind, msg))
}

#[async_trait]
impl FileSystemExt for BashkitFsOverHaest {}

#[async_trait]
impl FileSystem for BashkitFsOverHaest {
    async fn read_file(&self, path: &Path) -> Result<Vec<u8>> {
        self.fs
            .read(&normalize(path))
            .await
            .map_err(bashkit_from_io)
    }

    async fn write_file(&self, path: &Path, content: &[u8]) -> Result<()> {
        let p = normalize(path);
        self.fs.write(&p, content).await.map_err(bashkit_from_io)
    }

    async fn append_file(&self, path: &Path, content: &[u8]) -> Result<()> {
        let p = normalize(path);
        self.fs.append(&p, content).await.map_err(bashkit_from_io)
    }

    async fn mkdir(&self, path: &Path, recursive: bool) -> Result<()> {
        let p = normalize(path);
        // bashkit's contract, whatever the Fs does: an existing entry fails,
        // unless it is a directory and the call is recursive.
        if let Ok(st) = self.fs.stat(&p).await {
            return if st.is_dir() && recursive {
                Ok(())
            } else {
                Err(err(ErrorKind::AlreadyExists, "already exists"))
            };
        }
        let made = if recursive {
            self.fs.create_dir_all(&p).await
        } else {
            self.fs.create_dir(&p).await
        };
        made.map_err(bashkit_from_io)
    }

    async fn remove(&self, path: &Path, recursive: bool) -> Result<()> {
        let p = normalize(path);
        let removed = if recursive {
            self.fs.remove_all(&p).await
        } else {
            self.fs.remove(&p).await
        };
        removed.map_err(bashkit_from_io)?;
        self.forget_modes(&p, None);
        Ok(())
    }

    async fn stat(&self, path: &Path) -> Result<Metadata> {
        let p = normalize(path);
        let st = self.stat_path(&p).await?;
        Ok(self.metadata(&p, &st))
    }

    async fn read_dir(&self, path: &Path) -> Result<Vec<DirEntry>> {
        let p = normalize(path);
        let entries = self.fs.read_dir(&p).await.map_err(bashkit_from_io)?;
        let mut out = Vec::with_capacity(entries.len());
        for e in entries {
            let child = p.join(&e.name);
            // Listing metadata must match `stat` (bashkit callers trust it); a
            // dangling entry keeps the kind the listing gave it.
            let st = self
                .fs
                .stat(&child)
                .await
                .unwrap_or_else(|_| FsStat::new(e.kind, 0));
            out.push(DirEntry {
                metadata: self.metadata(&child, &st),
                name: e.name,
            });
        }
        Ok(out)
    }

    async fn exists(&self, path: &Path) -> Result<bool> {
        Ok(self.fs.exists(&normalize(path)).await)
    }

    async fn rename(&self, from: &Path, to: &Path) -> Result<()> {
        let (from, to) = (normalize(from), normalize(to));
        self.fs.rename(&from, &to).await.map_err(bashkit_from_io)?;
        self.forget_modes(&from, Some(&to));
        Ok(())
    }

    async fn copy(&self, from: &Path, to: &Path) -> Result<()> {
        let (from, to) = (normalize(from), normalize(to));
        if self.stat_path(&from).await?.is_dir() {
            return Err(err(ErrorKind::IsADirectory, "is a directory"));
        }
        let data = self.fs.read(&from).await.map_err(bashkit_from_io)?;
        self.fs.write(&to, &data).await.map_err(bashkit_from_io)
    }

    async fn symlink(&self, _target: &Path, _link: &Path) -> Result<()> {
        Err(err(
            ErrorKind::Unsupported,
            "symbolic links are not supported by this file system",
        ))
    }

    async fn read_link(&self, path: &Path) -> Result<PathBuf> {
        self.fs
            .read_link(&normalize(path))
            .await
            .map_err(bashkit_from_io)
    }

    async fn chmod(&self, path: &Path, mode: u32) -> Result<()> {
        let p = normalize(path);
        self.stat_path(&p).await?;
        self.modes().insert(p, mode);
        Ok(())
    }

    async fn set_modified_time(&self, path: &Path, _time: SystemTime) -> Result<()> {
        self.stat_path(&normalize(path)).await.map(|_| ())
    }
}
