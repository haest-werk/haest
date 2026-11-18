//! A file the apply writes, put on the disk whole or not at all. The
//! sandbox's bytes go into a new file beside the real one, which takes the
//! real one's permission bits and is then renamed over it, so a disk that
//! fills up, or a kill, partway leaves the human's file as it was. Where a
//! new file would not be the same file, the bytes are written in place, as
//! before. An apply dropped partway (Ctrl+C at the haest command's
//! `/apply`, a host's timeout) removes the new file as it drops.

use std::{
    io,
    os::unix::fs::{MetadataExt, PermissionsExt},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

use haest_core::fs::Fs;

/// The permission bits a replaced file keeps: all but setuid and setgid,
/// which a write into the file takes away as well.
const KEPT: u32 = 0o1777;

/// The longest part of a file's name its new file's name repeats, so a long
/// one leaves room for the rest under the system's cap on a name.
const NAME_PART: usize = 64;

/// Put `bytes` at the real path `real` on `disk`, whole or not at all.
///
/// A file there must still take a write: one the human made read-only is
/// refused as an in-place write is, never replaced. It is written in place
/// where a new file would not stand in for it: another name links to it,
/// it is not a regular file, the new file would have another owner or
/// group (another user's file the human may write, a directory that gives
/// new files its own group), or the directory takes no new file
/// (`PermissionDenied`). A new file the disk has no room for fails the
/// change, and never falls back to writing in place.
pub(super) async fn put_file(disk: &dyn Fs, real: &Path, bytes: &[u8]) -> io::Result<()> {
    let old = if disk.exists(real).await {
        // Opened to write as an in-place write opens it, but nothing is
        // cut or written: refused, the change fails as it did.
        disk.append(real, &[]).await?;
        // What the `Fs` does not carry; None on a disk that is not the
        // host's own (nothing to keep, then).
        tokio::fs::symlink_metadata(real).await.ok()
    } else {
        None
    };
    if old
        .as_ref()
        .is_some_and(|m| m.nlink() > 1 || !m.file_type().is_file())
    {
        return disk.write(real, bytes).await;
    }
    let Some(new) = new_beside(disk, real).await? else {
        return disk.write(real, bytes).await;
    };
    if let Some(old) = &old {
        let made = tokio::fs::symlink_metadata(&new.path).await.ok();
        if made.is_some_and(|m| (m.uid(), m.gid()) != (old.uid(), old.gid())) {
            let _ = disk.remove(&new.path).await;
            new.settled();
            return disk.write(real, bytes).await;
        }
    }
    let put = async {
        if old.is_some() {
            // Its owner's alone while it fills, so the bytes are never open
            // to more people than the file they replace is.
            disk.chmod(&new.path, 0o600).await?;
        }
        disk.write(&new.path, bytes).await?;
        if let Some(old) = &old {
            // The old file's mode, so the apply's own mode check
            // (`carry_mode`) reads it as the disk's.
            disk.chmod(&new.path, old.permissions().mode() & KEPT)
                .await?;
        }
        disk.rename(&new.path, real).await
    };
    let put = put.await;
    if put.is_err() {
        let _ = disk.remove(&new.path).await;
    }
    new.settled();
    put
}

/// The new file beside the real one, from just before it is made until it
/// is renamed over the real one or removed. An apply dropped meanwhile
/// drops it, and it removes the file: the apply leaves nothing beside the
/// human's file. The removal cannot wait, so it is the system's own, on
/// the host's disk, which the overlay's disk is; on a disk that is not,
/// nothing has the path (its name is this process's own).
struct NewFile {
    path: PathBuf,
    /// Still to rename or remove.
    pending: bool,
}

impl NewFile {
    /// Renamed over the real file, or removed: nothing left to remove.
    fn settled(mut self) {
        self.pending = false;
    }
}

impl Drop for NewFile {
    fn drop(&mut self) {
        if self.pending {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

/// A new, empty file beside `real`, under a dot-name nothing on the disk
/// has (`.<name>.haest-<pid>-<n>`); None where the directory takes no new
/// file.
async fn new_beside(disk: &dyn Fs, real: &Path) -> io::Result<Option<NewFile>> {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let name = real
        .file_name()
        .map(|n| n.to_string_lossy())
        .unwrap_or_default();
    let mut cut = name.len().min(NAME_PART);
    while !name.is_char_boundary(cut) {
        cut -= 1;
    }
    for _ in 0..100 {
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        let pid = std::process::id();
        let path = real.with_file_name(format!(".{}.haest-{pid}-{n}", &name[..cut]));
        // A link counts, dangling or not: a write would go where it points.
        if disk.read_link(&path).await.is_ok() || disk.exists(&path).await {
            continue;
        }
        // Held before the write that makes it: dropped there, it may be made.
        let new = NewFile {
            path,
            pending: true,
        };
        return match disk.write(&new.path, &[]).await {
            Ok(()) => Ok(Some(new)),
            Err(e) => {
                new.settled();
                match e.kind() {
                    io::ErrorKind::PermissionDenied => Ok(None),
                    _ => Err(e),
                }
            }
        };
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        format!("no free name beside {} to write it", real.display()),
    ))
}

#[cfg(test)]
mod tests {
    use std::{
        io,
        path::{Path, PathBuf},
        sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        },
    };

    use async_trait::async_trait;
    use haest_core::{
        fs::{DirEntry, Fs, FsRef, FsStat, MemFs},
        workspace::{ApplyFailureCode, Change, ChangeSet},
    };

    use crate::{RealFs, bridge::BashkitFsOverHaest, overlay::OverlayChanges};

    /// The real disk, filling up: a write past `room` bytes writes that
    /// many, then fails as a full disk does.
    struct Filling {
        room: AtomicUsize,
    }

    #[async_trait]
    impl Fs for Filling {
        async fn write(&self, p: &Path, d: &[u8]) -> io::Result<()> {
            let room = self.room.load(Ordering::SeqCst);
            if d.len() <= room {
                return RealFs.write(p, d).await;
            }
            RealFs.write(p, &d[..room]).await?;
            Err(io::Error::from_raw_os_error(libc::ENOSPC))
        }
        async fn read(&self, p: &Path) -> io::Result<Vec<u8>> {
            RealFs.read(p).await
        }
        async fn append(&self, p: &Path, d: &[u8]) -> io::Result<()> {
            RealFs.append(p, d).await
        }
        async fn stat(&self, p: &Path) -> io::Result<FsStat> {
            RealFs.stat(p).await
        }
        async fn read_link(&self, p: &Path) -> io::Result<PathBuf> {
            RealFs.read_link(p).await
        }
        async fn read_dir(&self, p: &Path) -> io::Result<Vec<DirEntry>> {
            RealFs.read_dir(p).await
        }
        async fn create_dir(&self, p: &Path) -> io::Result<()> {
            RealFs.create_dir(p).await
        }
        async fn create_dir_all(&self, p: &Path) -> io::Result<()> {
            RealFs.create_dir_all(p).await
        }
        async fn remove(&self, p: &Path) -> io::Result<()> {
            RealFs.remove(p).await
        }
        async fn remove_all(&self, p: &Path) -> io::Result<()> {
            RealFs.remove_all(p).await
        }
        async fn rename(&self, a: &Path, b: &Path) -> io::Result<()> {
            RealFs.rename(a, b).await
        }
        async fn chmod(&self, p: &Path, mode: u32) -> io::Result<()> {
            RealFs.chmod(p, mode).await
        }
    }

    fn conflicts(changes: &[Change]) -> Vec<(&str, bool)> {
        changes
            .iter()
            .map(|c| (c.path.as_str(), c.conflict))
            .collect()
    }

    fn names(dir: &Path) -> Vec<String> {
        let mut all: Vec<String> = std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        all.sort();
        all
    }

    /// The disk fills up while the apply writes the sandbox's `notes.txt`.
    /// The write had emptied the human's file first: the change failed,
    /// the human's text was gone, and since the disk then held neither
    /// version, every later scan listed it as a conflict and every apply
    /// skipped it.
    #[tokio::test]
    async fn a_write_the_full_disk_stops_leaves_the_humans_file_whole() {
        let t = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(t.path()).unwrap();
        let human = "the human's notes\n".repeat(50);
        let sandbox = "the sandbox's longer notes\n".repeat(100);
        std::fs::write(root.join("notes.txt"), &human).unwrap();
        let files = MemFs::from_files([("/notes.txt", human.as_str())]);
        let overlay = Arc::new(BashkitFsOverHaest::new(FsRef::new(files)));
        let disk = Arc::new(Filling {
            room: AtomicUsize::new(64),
        });
        let changes = OverlayChanges::new(overlay, disk.clone(), &root);
        let recorder = changes.recorder();
        let notes = Path::new("/notes.txt");
        recorder
            .write_file(notes, sandbox.as_bytes())
            .await
            .unwrap();

        let report = changes.apply().await;
        let failed: Vec<_> = report
            .failed
            .iter()
            .map(|f| (f.change.path.as_str(), f.code))
            .collect();
        assert_eq!(failed, [("/notes.txt", ApplyFailureCode::WriteFailed)]);
        let on_disk = std::fs::read_to_string(root.join("notes.txt")).unwrap();
        assert!(
            on_disk == human,
            "the human's file: {} bytes",
            on_disk.len()
        );
        assert_eq!(names(&root), ["notes.txt"], "nothing left beside it");
        assert_eq!(conflicts(&changes.list().await), [("/notes.txt", false)]);

        // Room is made: the next apply writes it.
        disk.room.store(usize::MAX, Ordering::SeqCst);
        let report = changes.apply().await;
        assert_eq!(conflicts(&report.applied), [("/notes.txt", false)]);
        let on_disk = std::fs::read_to_string(root.join("notes.txt")).unwrap();
        assert!(on_disk == sandbox, "the sandbox's: {} bytes", on_disk.len());
        assert!(changes.list().await.is_empty());
    }
}
