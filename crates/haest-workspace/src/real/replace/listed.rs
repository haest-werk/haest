//! The name a file that is replaced goes under: the one its directory
//! lists, however the write spelled it.

use std::{
    fs::Metadata,
    io,
    os::unix::fs::{DirEntryExt, MetadataExt},
    path::{Path, PathBuf},
};

use crate::listed::{Pick, confirmed, pick};

/// `real`, one regular file that is there with no other link (`old`), as
/// its directory lists it ([`crate::listed`]): under the asked name, or
/// else under the entry `stat` agrees is this file. A disk that reads two
/// spellings of a name as one and keeps the spelling a rename gives (HFS+,
/// exFAT and FAT on macOS) renames the human's `README.md` to `readme.md`
/// when a new file goes over it as `readme.md`.
///
/// None where the listing names neither: renamed over as asked, it could
/// take that name, so the bytes go in place. As asked where the directory
/// cannot be listed. Only the last name changes; the directory stays as
/// asked.
pub(super) async fn as_listed(real: &Path, old: &Metadata) -> Option<PathBuf> {
    let (Some(dir), Some(asked)) = (real.parent(), real.file_name()) else {
        return Some(real.to_path_buf());
    };
    let (listed, wanted, ino) = (dir.to_path_buf(), asked.to_os_string(), old.ino());
    let picked = tokio::task::spawn_blocking(move || {
        let listing = std::fs::read_dir(listed)?;
        Ok::<_, io::Error>(pick(
            &wanted,
            ino,
            listing.map(|e| e.map(|e| (e.file_name(), e.ino()))),
        ))
    })
    .await;
    let picked = match picked {
        Ok(Ok(picked)) => picked,
        Ok(Err(_)) | Err(_) => Pick::Asked,
    };
    let name = confirmed(dir, asked, picked, (old.dev(), old.ino())).await?;
    Some(real.with_file_name(name))
}
