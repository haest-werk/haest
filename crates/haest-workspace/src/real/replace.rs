//! The real disk's whole write ([`Fs::write_whole`] on
//! [`RealFs`](crate::RealFs)): a file put on the disk whole or not at all.
//! Its callers are the tools that rewrite a file in the real workspace
//! (Edit, Write over a file that is there, `GenerateImage` over an image
//! that is there), and the overlay's apply (`overlay/apply.rs`). The new
//! file is renamed onto the path it was given. On a disk that reads two
//! spellings of a name as one, the system decides which one the file then
//! goes by, as it does for any program that saves a file by renaming a new
//! one over it: APFS keeps the name it holds, HFS+, exFAT and FAT on macOS
//! take the one given.
//!
//! The bytes go into a new file beside the real one, which `tempfile`
//! makes under a name nothing on the disk has (`.haest-*`, `O_EXCL`), its
//! owner's alone from the moment it is made where it is to replace a file.
//! It then takes the real one's permission bits and is renamed over it, so
//! a disk that fills up, or a kill, partway leaves the human's file as it
//! was. Where a new file would not be the same file, or its directory
//! takes no new name, the bytes are written in place, as a plain write puts
//! them; anything else that stops the new file fails the write with the
//! human's file as it was. A write dropped partway (Ctrl+C at the haest
//! command's `/apply`, a host's timeout) removes the new file as it drops,
//! and what the runtime still writes then goes into a file no name
//! reaches; dropped once the rename is under way, it lands whole.
//!
//! [`Fs::write_whole`]: haest_core::fs::Fs::write_whole

use std::{
    fs::{File, Permissions},
    io::{self, Write},
    os::unix::fs::{MetadataExt, PermissionsExt},
    path::Path,
};

use super::write;

/// The permission bits a replaced file keeps: all but setuid and setgid,
/// which a write into the file takes away as well.
const KEPT: u32 = 0o1777;

/// Put `bytes` at the real path `real`, whole or not at all.
///
/// It is written in place where a new file would not stand in for the one
/// there: another name links to it, or it is not a regular file (a
/// symlink, a FIFO, a device). That one is opened once, as a plain write
/// opens it: a FIFO opened twice hands its reader an empty message first.
/// A regular file must still take a write: one the human made read-only
/// is refused as an in-place write is, never replaced. It is written in
/// place, too, where its directory takes no new name (made read-only).
/// Anything else that stops the new file (a full disk, a rename the disk
/// refuses) fails the write, the new file removed and the real one as it
/// was. The new file is removed on a drop as well.
pub(crate) async fn put_file(real: &Path, bytes: &[u8]) -> io::Result<()> {
    let old = match tokio::fs::symlink_metadata(real).await {
        Ok(m) if m.nlink() > 1 || !m.file_type().is_file() => {
            return write::write(real, bytes).await;
        }
        Ok(m) => {
            // Opened to write as an in-place write opens it, but nothing is
            // cut or written: refused, the write fails as a plain one would.
            write::append(real, &[]).await?;
            Some(m)
        }
        Err(_) => None,
    };
    let dir = real.parent().unwrap_or(Path::new(".")).to_owned();
    let replaces = old.is_some();
    let made = blocking(move || {
        let mut new = tempfile::Builder::new();
        new.prefix(".haest-");
        // Where nothing is replaced, the mode a plain write makes a file
        // with (the umask's); else 0600, tempfile's own.
        if !replaces {
            new.permissions(Permissions::from_mode(0o666));
        }
        new.tempfile_in(dir)
    })
    .await;
    let (file, path) = match made {
        Ok(new) => new.into_parts(),
        Err(e) if e.kind() == io::ErrorKind::PermissionDenied => {
            return write::write(real, bytes).await;
        }
        Err(e) => return Err(e),
    };
    let (data, mode) = (bytes.to_vec(), old.map(|m| m.mode() & KEPT));
    blocking(move || fill(file, &data, mode)).await?;
    let real = real.to_owned();
    blocking(move || path.persist(real).map_err(|e| e.error)).await
}

/// The sandbox's `bytes` into the new `file`, then `mode`, that of the file
/// it is to replace, so the apply's own mode check (`carry_mode`) reads it
/// as the disk's. Closed with close(2)'s answer (`write.rs`): an NFS server
/// out of room says so only there.
fn fill(mut file: File, bytes: &[u8], mode: Option<u32>) -> io::Result<()> {
    file.write_all(bytes)?;
    if let Some(mode) = mode {
        file.set_permissions(Permissions::from_mode(mode))?;
    }
    write::close(file)
}

/// `f` on the runtime's blocking pool, where tokio runs its own file calls.
async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> io::Result<T> + Send + 'static,
) -> io::Result<T> {
    tokio::task::spawn_blocking(f)
        .await
        .map_err(io::Error::other)?
}

#[cfg(test)]
mod tests;
