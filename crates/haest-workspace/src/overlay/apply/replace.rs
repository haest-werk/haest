//! A file the apply writes, put on the disk whole or not at all. The
//! sandbox's bytes go into a new file, in a directory of its own beside the
//! real one that only its owner may enter; the new file takes the real
//! one's permission bits and is then renamed over it, so a disk that fills
//! up, or a kill, partway leaves the human's file as it was. Where a new
//! file would not be the same file, the bytes are written in place, as
//! before. An apply dropped partway (Ctrl+C at the haest command's
//! `/apply`, a host's timeout) removes the new file and its directory as
//! it drops.

use std::{
    fs::Metadata,
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
    let Some(new) = NewFile::beside(disk, real).await? else {
        return disk.write(real, bytes).await;
    };
    if let Some(old) = &old {
        let made = tokio::fs::symlink_metadata(&new.path).await.ok();
        if made.is_some_and(|m| (m.uid(), m.gid()) != (old.uid(), old.gid())) {
            new.clear(disk).await;
            return disk.write(real, bytes).await;
        }
    }
    let put = async {
        fill(disk, &new.path, old.as_ref(), bytes).await?;
        disk.rename(&new.path, real).await
    };
    let put = put.await;
    new.clear(disk).await;
    put
}

/// The sandbox's `bytes` into the new file at `path`, with the mode of
/// `old`, the file it is to replace.
async fn fill(disk: &dyn Fs, path: &Path, old: Option<&Metadata>, bytes: &[u8]) -> io::Result<()> {
    disk.write(path, bytes).await?;
    if let Some(old) = old {
        // The old file's mode, so the apply's own mode check
        // (`carry_mode`) reads it as the disk's.
        disk.chmod(path, old.permissions().mode() & KEPT).await?;
    }
    Ok(())
}

/// The new file and its directory beside the real file, from just before
/// the directory is made until both are gone. An apply dropped meanwhile
/// drops it, and it removes them: the apply leaves nothing beside the
/// human's file. A write the runtime still holds then (dropping its future
/// does not stop it) finds no directory to land in; one it is running lands
/// in the directory, where only its owner may reach it. The removal cannot
/// wait, so it is the system's own, on the host's disk, which the overlay's
/// disk is; on a disk that is not, nothing has the path (its name is this
/// process's own).
struct NewFile {
    /// Only its owner may enter it, before anything goes in.
    dir: PathBuf,
    path: PathBuf,
    /// Still to remove.
    pending: bool,
}

impl NewFile {
    /// A new, empty file, in a directory of its own beside `real` under a
    /// dot-name nothing on the disk has (`.<name>.haest-<pid>-<n>`); None
    /// where the directory takes no new name.
    async fn beside(disk: &dyn Fs, real: &Path) -> io::Result<Option<Self>> {
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
            let dir = real.with_file_name(format!(".{}.haest-{pid}-{n}", &name[..cut]));
            // Held before the create that makes it: dropped there, it may
            // be made.
            let new = Self {
                path: dir.join("new"),
                dir,
                pending: true,
            };
            match disk.create_dir(&new.dir).await {
                Ok(()) => {}
                Err(e) => {
                    // Not made, or another's (a name taken, a link
                    // included, dangling or not): nothing to remove.
                    new.settled();
                    match e.kind() {
                        io::ErrorKind::AlreadyExists => continue,
                        io::ErrorKind::PermissionDenied => return Ok(None),
                        _ => return Err(e),
                    }
                }
            }
            let made = async {
                // A group the directory passes on (setgid) stays, so the
                // new file gets the group one made beside the real file
                // would.
                let setgid = tokio::fs::symlink_metadata(&new.dir)
                    .await
                    .map_or(0, |m| m.permissions().mode() & 0o2000);
                disk.chmod(&new.dir, 0o700 | setgid).await?;
                disk.write(&new.path, &[]).await
            };
            return match made.await {
                Ok(()) => Ok(Some(new)),
                Err(e) => {
                    new.clear(disk).await;
                    Err(e)
                }
            };
        }
        Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            format!("no free name beside {} to write it", real.display()),
        ))
    }

    /// Remove the file (unless it was renamed over the real one) and its
    /// directory; dropped partway, the drop does.
    async fn clear(self, disk: &dyn Fs) {
        let _ = disk.remove(&self.path).await;
        let _ = disk.remove(&self.dir).await;
        self.settled();
    }

    /// Removed, or never made: nothing left to remove.
    fn settled(mut self) {
        self.pending = false;
    }
}

impl Drop for NewFile {
    fn drop(&mut self) {
        if self.pending {
            let _ = std::fs::remove_file(&self.path);
            let _ = std::fs::remove_dir(&self.dir);
        }
    }
}

#[cfg(test)]
mod tests;
