//! The name a file that is replaced goes under: the one its directory
//! lists, however the write spelled it.

use std::{
    ffi::{OsStr, OsString},
    fs::Metadata,
    io,
    os::unix::fs::{DirEntryExt, MetadataExt},
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
/// The asked name wins wherever the directory lists it. Another entry is
/// taken only where `stat` agrees it is this file (same device and inode):
/// the number a listing gives is the file system's word, and one that
/// gives a number `stat` does not could name another file.
///
/// None where neither holds (an empty file on exFAT or FAT gets a new
/// inode number on each look): renamed over as asked, it could take that
/// name, so the bytes go in place. As asked where the directory cannot be
/// listed. Only the last name changes; the directory stays as asked.
pub(super) async fn as_listed(real: &Path, old: &Metadata) -> Option<PathBuf> {
    let (Some(dir), Some(asked)) = (real.parent(), real.file_name()) else {
        return Some(real.to_path_buf());
    };
    let (dir, wanted, ino) = (dir.to_path_buf(), asked.to_os_string(), old.ino());
    let picked = tokio::task::spawn_blocking(move || {
        let listing = std::fs::read_dir(dir)?;
        Ok::<_, io::Error>(pick(
            &wanted,
            ino,
            listing.map(|e| e.map(|e| (e.file_name(), e.ino()))),
        ))
    })
    .await;
    let other = match picked {
        Ok(Ok(Pick::Other(name))) => name,
        Ok(Ok(Pick::Neither)) => return None,
        Ok(Ok(Pick::Asked) | Err(_)) | Err(_) => return Some(real.to_path_buf()),
    };
    let listed = real.with_file_name(other);
    match tokio::fs::symlink_metadata(&listed).await {
        Ok(m) if (m.dev(), m.ino()) == (old.dev(), old.ino()) => Some(listed),
        _ => None,
    }
}

/// What a directory's listing says of one file.
#[derive(Debug, PartialEq, Eq)]
enum Pick {
    /// Listed under the asked name, or the listing could not be read to the
    /// end: as asked.
    Asked,
    /// Not listed as asked; the first entry with its inode number.
    Other(OsString),
    /// Listed by neither.
    Neither,
}

/// Walk `listing`, `(name, inode number)` in the directory's order, for the
/// file asked for as `asked` with inode number `ino`. The asked name
/// settles it wherever it comes, ahead of an entry with the number.
fn pick(
    asked: &OsStr,
    ino: u64,
    listing: impl IntoIterator<Item = io::Result<(OsString, u64)>>,
) -> Pick {
    let mut by_number = None;
    for entry in listing {
        let Ok((name, number)) = entry else {
            return Pick::Asked;
        };
        if name == asked {
            return Pick::Asked;
        }
        if by_number.is_none() && number == ino {
            by_number = Some(name);
        }
    }
    by_number.map_or(Pick::Neither, Pick::Other)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn walk(asked: &str, ino: u64, listing: &[(&str, u64)]) -> Pick {
        let listing = listing.iter().map(|(n, i)| Ok((OsString::from(n), *i)));
        pick(OsStr::new(asked), ino, listing)
    }

    /// A listing that gives another entry the file's number ahead of the
    /// file's own name never sends the file over that other entry.
    #[test]
    fn the_asked_name_wins_over_an_earlier_entry_with_its_number() {
        let listing = [("notes.md", 7), ("README.md", 9)];
        assert_eq!(walk("README.md", 7, &listing), Pick::Asked);
    }

    /// The file listed under another spelling only: that entry, to check.
    #[test]
    fn another_spelling_is_found_by_its_number() {
        let listing = [("TODO.md", 3), ("README.md", 7)];
        assert_eq!(
            walk("readme.md", 7, &listing),
            Pick::Other("README.md".into())
        );
    }

    /// Neither the name nor the number: nothing to go under.
    #[test]
    fn a_file_listed_by_neither_has_no_name() {
        assert_eq!(walk("readme.md", 7, &[("TODO.md", 3)]), Pick::Neither);
    }

    /// A listing that breaks off partway: as asked, as before the walk.
    #[test]
    fn a_listing_that_breaks_off_leaves_the_name_as_asked() {
        let listing = [
            Ok(("TODO.md".into(), 3)),
            Err(io::Error::other("gone")),
            Ok(("README.md".into(), 7)),
        ];
        assert_eq!(pick(OsStr::new("readme.md"), 7, listing), Pick::Asked);
    }
}
