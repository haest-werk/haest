//! Whether a directory reads two spellings of a name as one: known once
//! per directory, and never taken from a name that cannot tell.

use std::sync::atomic::{AtomicUsize, Ordering};

use super::*;

/// `n` files named by number alone (`00`, `01`, ...) in `dir`: no ASCII
/// letter to swap the case of.
fn numbered(dir: &Path, n: usize) {
    for i in 0..n {
        fs::write(dir.join(format!("{i:02}")), "").unwrap();
    }
}

/// A name with no ASCII letter cannot be asked under another case, so on
/// Linux the listing decided each one; and a directory something keeps
/// writing to keeps no listing: each lookup read the whole directory again,
/// and `grep -r` over 5,000 numbered files ran into its time cap where 1 s
/// did. A directory that tells case apart is known as one once, and a name
/// in it is there as `realpath` gives it.
#[tokio::test]
async fn numbered_names_in_a_directory_that_tells_case_apart_read_no_listing_each() {
    let t = tempfile::tempdir().unwrap();
    let dir = t.path();
    numbered(dir, 100);
    let s = Spelling {
        settled: Duration::MAX,
        ..Spelling::of(dir).telling_case_apart()
    };
    for i in 0..100 {
        let asked = format!("/{i:02}");
        assert_eq!(s.spelled(Path::new(&asked)).await, Spelled::AsAsked);
    }
    assert!(s.reads() <= 1, "{} listings read", s.reads());
}

/// The disk is asked once, and a directory once, however many names are
/// looked up in it: `statfs` ran for each name of each lookup. (Where the
/// system gives no birth time, a directory's flag is read at each lookup:
/// its inode number alone may be another directory's by then.)
#[tokio::test]
async fn a_disk_and_a_directory_are_asked_once_for_every_name_in_them() {
    static DISKS: AtomicUsize = AtomicUsize::new(0);
    static FLAGS: AtomicUsize = AtomicUsize::new(0);
    let t = tempfile::tempdir().unwrap();
    let dir = t.path();
    fs::create_dir(dir.join("lib")).unwrap();
    numbered(&dir.join("lib"), 50);
    let s = Spelling {
        answers: Answers {
            disk: |_| {
                DISKS.fetch_add(1, Ordering::Relaxed);
                Disk::Flagged
            },
            flag: |_| {
                FLAGS.fetch_add(1, Ordering::Relaxed);
                Some(false)
            },
            ..Answers::SYSTEM
        },
        ..Spelling::of(dir)
    };
    for i in 0..50 {
        let asked = format!("/lib/{i:02}");
        assert_eq!(s.spelled(Path::new(&asked)).await, Spelled::AsAsked);
    }
    assert_eq!(DISKS.load(Ordering::Relaxed), 1, "disks asked");
    // The project directory's flag, and `lib`'s.
    let flags = FLAGS.load(Ordering::Relaxed);
    if fs::metadata(dir).unwrap().created().is_ok() {
        assert_eq!(flags, 2, "flags read");
    } else {
        assert_eq!(flags, 100, "flags read");
    }
}

/// Where nothing says how a directory spells (no flag, and only numbered
/// names asked in it), the one entry of its listing with an ASCII letter
/// tells, once: the listing is read for the first lookup alone where the
/// directory tells case apart, and decides each one where it folds names.
#[tokio::test]
async fn a_lettered_entry_of_its_listing_tells_how_a_directory_spells() {
    let t = tempfile::tempdir().unwrap();
    let dir = t.path();
    numbered(dir, 100);
    fs::write(dir.join("notes.md"), "").unwrap();
    let s = Spelling {
        settled: Duration::MAX,
        answers: Answers {
            disk: |_| Disk::Unknown,
            ..Answers::SYSTEM
        },
        ..Spelling::of(dir)
    };
    for i in 0..100 {
        let asked = format!("/{i:02}");
        assert_eq!(s.spelled(Path::new(&asked)).await, Spelled::AsAsked);
    }
    if dir.join("NOTES.md").exists() {
        assert_eq!(s.reads(), 100);
    } else {
        assert_eq!(s.reads(), 1);
    }
}

/// A directory that reads two spellings as one (Linux's `chattr +F`, which
/// folds every letter, not only ASCII ones) is never taken at `realpath`'s
/// word, also for a name with no ASCII letter: there `realpath` gives back
/// the spelling asked, and `mv ΑΒΓ αβγ` would delete the only file.
#[tokio::test]
async fn a_folding_directory_spells_a_name_with_no_ascii_letter_as_listed() {
    let t = tempfile::tempdir().unwrap();
    let dir = t.path();
    fs::write(dir.join("\u{391}\u{392}\u{393}"), "").unwrap();
    numbered(dir, 3);
    let s = Spelling {
        answers: Answers {
            disk: |_| Disk::Flagged,
            flag: |_| Some(true),
            ..echoing(dir).answers
        },
        ..Spelling::of(dir)
    };
    for asked in ["/00", "/02", "/\u{391}\u{392}\u{393}"] {
        let got = s.spelled(Path::new(asked)).await;
        assert_eq!(got, Spelled::AsAsked, "{asked}");
    }
    let lower = "/\u{3b1}\u{3b2}\u{3b3}";
    let want = if dir.join(&lower[1..]).exists() {
        Spelled::Other(Some(PathBuf::from("/\u{391}\u{392}\u{393}")))
    } else {
        Spelled::AsAsked
    };
    assert_eq!(s.spelled(Path::new(lower)).await, want);
}
