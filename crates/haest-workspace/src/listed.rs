//! THE NAME A DIRECTORY LISTS A FILE BY, however a path spelled it.
//!
//! On a disk that reads two spellings of a name as one (macOS's default,
//! exFAT, FAT, vfat), the listing is the one record of the name the disk
//! holds: `realpath` answers there with the spelling a file was first looked
//! up by (exFAT and FAT on macOS) or with the one asked (Linux). The
//! sandbox's spelling (`spelling.rs`) and the real disk's whole write
//! (`real/replace/listed.rs`) both ask it here.

use std::{
    ffi::{OsStr, OsString},
    io,
    os::unix::fs::MetadataExt,
    path::Path,
};

/// What a directory's listing says of one file.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Pick {
    /// Listed under the asked name, or the listing could not be read to the
    /// end: as asked.
    Asked,
    /// Not listed as asked; the one other entry that may be the file, still
    /// to check ([`confirmed`]).
    Other(OsString),
    /// Listed by neither: no other entry may be the file, or more than one.
    Neither,
}

/// Walk `listing`, `(name, inode number)` in any order, for the file asked
/// for as `asked` whose inode number is `ino`. The asked name settles it
/// wherever it comes. Otherwise the file may be the one entry with its
/// number, or else the one whose name is the asked one but for the case of
/// its ASCII letters: an empty file on exFAT or FAT has no number of its
/// own, and its listing gives it a new one on each read.
pub(crate) fn pick<N: AsRef<OsStr>>(
    asked: &OsStr,
    ino: u64,
    listing: impl IntoIterator<Item = io::Result<(N, u64)>>,
) -> Pick {
    let (mut by_number, mut by_case) = (One::None, One::None);
    for entry in listing {
        let Ok((name, number)) = entry else {
            return Pick::Asked;
        };
        let name = name.as_ref();
        if name == asked {
            return Pick::Asked;
        }
        if number == ino {
            by_number.add(name);
        }
        if name.eq_ignore_ascii_case(asked) {
            by_case.add(name);
        }
    }
    by_number
        .only()
        .or_else(|| by_case.only())
        .map_or(Pick::Neither, Pick::Other)
}

/// The entries a walk found that may be the file.
enum One {
    None,
    Only(OsString),
    Many,
}

impl One {
    fn add(&mut self, name: &OsStr) {
        *self = match self {
            Self::None => Self::Only(name.to_os_string()),
            Self::Only(_) | Self::Many => Self::Many,
        };
    }

    fn only(self) -> Option<OsString> {
        match self {
            Self::Only(name) => Some(name),
            Self::None | Self::Many => None,
        }
    }
}

/// The name `dir` lists the file asked for as `asked` by, from what [`pick`]
/// found: the asked one, or the other entry, taken only where `lstat` of it
/// gives `id`, the file's own device and inode number. A number a listing
/// gives is the file system's word, and one `stat` does not agree with
/// could name another file. None where neither holds.
pub(crate) async fn confirmed(
    dir: &Path,
    asked: &OsStr,
    picked: Pick,
    id: (u64, u64),
) -> Option<OsString> {
    match picked {
        Pick::Asked => Some(asked.to_os_string()),
        Pick::Neither => None,
        Pick::Other(name) => match tokio::fs::symlink_metadata(dir.join(&name)).await {
            Ok(m) if (m.dev(), m.ino()) == id => Some(name),
            _ => None,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn walk(asked: &str, ino: u64, listing: &[(&str, u64)]) -> Pick {
        let listing = listing.iter().map(|(n, i)| Ok((OsStr::new(n), *i)));
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

    /// An empty file on exFAT or FAT: the listing's number is never the
    /// file's own, and the name but for its case is the only lead.
    #[test]
    fn a_file_whose_number_is_not_listed_is_found_by_its_name_but_for_case() {
        let listing = [("todo.txt", 5), ("TODO.md", u64::MAX - 9)];
        assert_eq!(
            walk("todo.md", u64::MAX - 1, &listing),
            Pick::Other("TODO.md".into())
        );
    }

    /// Two names linked to one file: the number names neither alone, the
    /// name but for its case does.
    #[test]
    fn a_number_two_entries_share_is_no_lead() {
        let listing = [("b.md", 7), ("A.md", 7)];
        assert_eq!(walk("a.md", 7, &listing), Pick::Other("A.md".into()));
        assert_eq!(walk("c.md", 7, &listing), Pick::Neither);
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
            Ok(("TODO.md", 3)),
            Err(io::Error::other("gone")),
            Ok(("README.md", 7)),
        ];
        assert_eq!(pick(OsStr::new("readme.md"), 7, listing), Pick::Asked);
    }
}
