//! The real disk's whole write ([`Fs::write_whole`]): a file put on the
//! disk whole or not at all. Its callers are the tools that rewrite a file
//! in the real workspace through [`RealFs`](crate::RealFs) (Edit, Write
//! over a file that is there, `GenerateImage` over an image that is
//! there), and the overlay's apply (`overlay/apply.rs`), over whatever disk
//! the apply was handed. The new file is renamed onto the path it was
//! given. On a disk that reads two spellings of a name as one, the system
//! decides which one the file then goes by, as it does for any program
//! that saves a file by renaming a new one over it: APFS keeps the name it
//! holds, HFS+, exFAT and FAT on macOS take the one given.
//!
//! The bytes go into a new file, in a directory of its own beside the real
//! one that only its owner may enter from the moment it is made; a new file
//! that replaces one is its owner's alone too before they go in. It then
//! takes the real one's permission bits and is renamed over it, so a disk
//! that fills up, or a kill, partway leaves the human's file as it was.
//! Where a new file would not be the same file, or the disk will not make
//! one, make it its owner's alone or move it there, the bytes are written
//! in place, as a plain write puts them; where the new file's bytes do not
//! all go in, the write fails instead, since in place they would cut the
//! human's file short. A write dropped partway (Ctrl+C at the haest
//! command's `/apply`, a host's timeout) removes the new file and its
//! directory as it drops.

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
/// It is written in place where a new file would not stand in for the one
/// there: another name links to it, or it is not a regular file (a
/// symlink, a FIFO, a device). That one is opened once, as a plain write
/// opens it: a FIFO opened twice hands its reader an empty message first.
/// A regular file must still take a write: one the human made read-only
/// is refused as an in-place write is, never replaced. It is written in
/// place, too, where the new file would have another owner or group
/// (another user's file the human may write, a directory that gives new
/// files its own group), or where the disk will not make the new file,
/// make it its owner's alone, or move it over the real one
/// ([`in_place_instead`]). Where the new file's bytes do not all go in (a
/// full disk), the write fails and never falls back: in place they would
/// not all go in either, and the human's file would be cut short.
pub(crate) async fn put_file(disk: &dyn Fs, real: &Path, bytes: &[u8]) -> io::Result<()> {
    let old = if disk.exists(real).await {
        // What the `Fs` does not carry; None on a disk that is not the
        // host's own (nothing to keep, then).
        let old = tokio::fs::symlink_metadata(real).await.ok();
        if old
            .as_ref()
            .is_some_and(|m| m.nlink() > 1 || !m.file_type().is_file())
        {
            return disk.write(real, bytes).await;
        }
        // Opened to write as an in-place write opens it, but nothing is
        // cut or written: refused, the write fails as a plain one would.
        disk.append(real, &[]).await?;
        old
    } else {
        None
    };
    match put_new(disk, real, old.as_ref(), bytes).await? {
        Put::Replaced => Ok(()),
        Put::InPlace => disk.write(real, bytes).await,
    }
}

/// What became of the new file, gone either way.
enum Put {
    /// Renamed over the real file.
    Replaced,
    /// Not put in its place: the real file is to take the bytes in place.
    InPlace,
}

/// Put `bytes` in a new file beside `real`, which holds `old`, and rename
/// it over it. What it makes it removes again, whatever comes.
async fn put_new(
    disk: &dyn Fs,
    real: &Path,
    old: Option<&Metadata>,
    bytes: &[u8],
) -> io::Result<Put> {
    let new = match NewFile::beside(disk, real).await {
        Ok(new) => new,
        Err(e) if in_place_instead(&e) => return Ok(Put::InPlace),
        Err(e) => return Err(e),
    };
    if let Some(old) = old {
        let made = tokio::fs::symlink_metadata(&new.path).await.ok();
        if made.is_some_and(|m| (m.uid(), m.gid()) != (old.uid(), old.gid())) {
            new.clear(disk).await;
            return Ok(Put::InPlace);
        }
        // Its owner's alone before any byte goes in. A disk that will not
        // set its mode (a share mounted for one fixed owner the run is
        // not) is one that will not make it so: the bytes go in place.
        if let Err(e) = disk.chmod(&new.path, 0o600).await {
            new.clear(disk).await;
            return if in_place_instead(&e) {
                Ok(Put::InPlace)
            } else {
                Err(e)
            };
        }
    }
    if let Err(e) = fill(disk, &new.path, old, bytes).await {
        new.clear(disk).await;
        return Err(e);
    }
    let moved = disk.rename(&new.path, real).await;
    new.clear(disk).await;
    match moved {
        Ok(()) => Ok(Put::Replaced),
        Err(e) if in_place_instead(&e) => Ok(Put::InPlace),
        Err(e) => Err(e),
    }
}

/// Whether the disk's refusal to make the new file, to make it its owner's
/// alone, or to move it over the real one, leaves the real file to take
/// the bytes in place, which needs neither a new name, a new mode nor a
/// move:
/// - `PermissionDenied`: a directory that takes no new name, a mode the
///   disk will not set (a share mounted for one fixed owner the run is
///   not), or a rename an ACL or a security module forbids;
/// - `InvalidFilename`: the new file's longer path, past the system's cap;
/// - `ReadOnlyFilesystem`: a read-only mount around a file on a writable
///   mount of its own;
/// - `ResourceBusy`, `CrossesDevices`: a file that is a mount point of its
///   own (one file bind-mounted into the project), which nothing can be
///   renamed over;
/// - `Unsupported`: a file system that has no such call at all (`ENOSYS`,
///   `ENOTSUP`: a FUSE one without setattr cannot set a mode).
///
/// Anything else fails the write, a disk with no room above all: the
/// bytes would not all go in in place either, and the human's file would
/// be cut short.
fn in_place_instead(e: &io::Error) -> bool {
    matches!(
        e.kind(),
        io::ErrorKind::PermissionDenied
            | io::ErrorKind::InvalidFilename
            | io::ErrorKind::ReadOnlyFilesystem
            | io::ErrorKind::ResourceBusy
            | io::ErrorKind::CrossesDevices
            | io::ErrorKind::Unsupported
    )
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
/// the directory is made until both are gone. A whole write dropped
/// meanwhile (an apply, a tool's) drops it, and it removes them: nothing
/// is left beside the human's file. A write the runtime still holds then
/// (dropping its future does not stop it) finds no directory to land in,
/// and the file of one it is running goes with the directory. Only a
/// directory the runtime is still to make is left, empty. The removal
/// cannot wait, so it is the system's own, on the host's disk, which the
/// real disk is; on a disk that is not, nothing has the path (its name is
/// this process's own).
struct NewFile {
    /// Only its owner may enter it, from the moment it is made; its mode
    /// is never set after, so one made where the real file's directory
    /// passes its group on (setgid) keeps that bit and that group.
    dir: PathBuf,
    path: PathBuf,
    /// Still to remove.
    pending: bool,
}

impl NewFile {
    /// A new, empty file, in a directory of its own beside `real` under a
    /// dot-name nothing on the disk has (`.<name>.haest-<pid>-<n>`).
    async fn beside(disk: &dyn Fs, real: &Path) -> io::Result<Self> {
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
            // Owner-only as it is made, its mode never set after: in a
            // directory that passes its group on (setgid), it inherits the
            // bit, a chmod to 0700 clears it, and a run not in that group
            // cannot set it again (Linux). The new file would then get the
            // run's own group, not the one a file made beside the real one
            // gets.
            if let Err(e) = disk.create_private_dir(&new.dir).await {
                // Not made, or another's (a name taken, a link included,
                // dangling or not): nothing to remove.
                new.settled();
                if e.kind() == io::ErrorKind::AlreadyExists {
                    continue;
                }
                return Err(e);
            }
            return match disk.write(&new.path, &[]).await {
                Ok(()) => Ok(new),
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
            if std::fs::remove_dir(&self.dir).is_err() {
                // A write the system was running as the drop came made the
                // file between the two: it is made now, so it goes.
                let _ = std::fs::remove_file(&self.path);
                let _ = std::fs::remove_dir(&self.dir);
            }
        }
    }
}

#[cfg(test)]
mod tests;
