//! The name a file that is replaced goes under: the one its directory
//! lists, however the write spelled it.

use std::{
    fs::Metadata,
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
};

/// `real`, one regular file that is there with no other link (`old`), as
/// its directory lists it: under the asked name, or else under the name
/// of the entry with its inode. A disk that reads two spellings of a name
/// as one and keeps the spelling a rename gives (HFS+, exFAT and FAT on
/// macOS) renames the human's `README.md` to `readme.md` when a new file
/// goes over it as `readme.md`. The listing is the one record of the name
/// there: on exFAT and FAT `realpath` answers with the spelling the file
/// was first looked up by, and on Linux with the asked one.
///
/// None where the listing holds neither (an empty file on exFAT or FAT
/// gets a new inode number on each look): renamed over as asked, it could
/// take that name, so the bytes go in place. As asked where the directory
/// cannot be listed. Only the last name changes; the directory stays as
/// asked.
pub(super) async fn as_listed(real: &Path, old: &Metadata) -> Option<PathBuf> {
    let (Some(dir), Some(asked)) = (real.parent(), real.file_name()) else {
        return Some(real.to_path_buf());
    };
    let Ok(mut entries) = tokio::fs::read_dir(dir).await else {
        return Some(real.to_path_buf());
    };
    loop {
        match entries.next_entry().await {
            Ok(Some(e)) if e.file_name() == asked || e.ino() == old.ino() => {
                return Some(real.with_file_name(e.file_name()));
            }
            Ok(Some(_)) => {}
            Ok(None) => return None,
            Err(_) => return Some(real.to_path_buf()),
        }
    }
}
