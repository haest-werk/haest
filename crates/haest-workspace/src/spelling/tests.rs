use std::{fs, os::unix::fs::symlink};

use super::{folding::Disk, *};

#[tokio::test]
async fn a_path_reads_as_asked_only_by_the_disks_own_spelling() {
    let t = tempfile::tempdir().unwrap();
    let dir = t.path();
    fs::create_dir(dir.join("lib")).unwrap();
    fs::write(dir.join("lib/caf\u{e9}"), "").unwrap();
    symlink("lib", dir.join("link")).unwrap();
    let s = Spelling::of(dir);
    let asked_as_spelled = [
        "/",
        "/lib",
        "/lib/caf\u{e9}",
        "/link/caf\u{e9}",
        "/nope",
        "/lib/../x",
    ];
    for asked in asked_as_spelled {
        let got = s.spelled(Path::new(asked)).await;
        assert_eq!(got, Spelled::AsAsked, "{asked}");
    }
    // Each other spelling, where this disk reads it as the same name.
    for asked in ["/LIB/caf\u{e9}", "/lib/cafe\u{301}"] {
        let want = if dir.join(&asked[1..]).exists() {
            Spelled::Other(Some(PathBuf::from("/lib/caf\u{e9}")))
        } else {
            Spelled::AsAsked
        };
        assert_eq!(s.spelled(Path::new(asked)).await, want, "{asked}");
    }
}

#[cfg(target_os = "linux")]
mod disks;
mod folding;

/// What exFAT and FAT answer on macOS (the spelling a file was first
/// looked up by: on a fresh mount, the one asked) and vfat on Linux (the
/// spelling asked): `realpath` gives back the last name as asked, and
/// nothing tells whether the directory lists it so.
fn echoing(dir: &Path) -> Spelling {
    fn echo(p: &Path) -> io::Result<PathBuf> {
        fs::symlink_metadata(p)?;
        let parent = fs::canonicalize(p.parent().unwrap_or(p))?;
        Ok(parent.join(p.file_name().unwrap_or_default()))
    }
    Spelling {
        answers: Answers {
            realpath: echo,
            disk: |_| Disk::Folds,
            flag: |_| None,
            ..Answers::SYSTEM
        },
        ..Spelling::of(dir)
    }
}

/// On exFAT and FAT on macOS, and vfat on Linux, the lookup of `/readme.md`
/// took `realpath`'s `readme.md` for the disk's spelling while the
/// directory listed `README.md`: the sandbox showed both files, and
/// applying `mv README.md readme.md` deleted the only one there was. The
/// listing decides, and the disk's spelling is the entry it lists for the
/// same file.
#[tokio::test]
async fn a_name_realpath_gives_back_as_asked_is_there_only_as_listed() {
    let t = tempfile::tempdir().unwrap();
    let dir = t.path();
    fs::write(dir.join("README.md"), "the human's\n").unwrap();
    fs::create_dir(dir.join("lib")).unwrap();
    fs::write(dir.join("lib/a.rs"), "").unwrap();
    let s = echoing(dir);
    for asked in ["/README.md", "/lib", "/lib/a.rs", "/nope"] {
        let got = s.spelled(Path::new(asked)).await;
        assert_eq!(got, Spelled::AsAsked, "{asked}");
    }
    for (asked, disk) in [
        ("/readme.md", "/README.md"),
        ("/LIB", "/lib"),
        ("/lib/A.rs", "/lib/a.rs"),
        ("/LIB/a.rs", "/lib/a.rs"),
    ] {
        let want = if dir.join(&asked[1..]).exists() {
            Spelled::Other(Some(PathBuf::from(disk)))
        } else {
            Spelled::AsAsked
        };
        assert_eq!(s.spelled(Path::new(asked)).await, want, "{asked}");
    }
}

/// `store` with `n` files, and `vendor`, a symlink to it, in `dir`.
fn linked_store(dir: &Path, n: usize) {
    fs::create_dir(dir.join("store")).unwrap();
    for i in 0..n {
        fs::write(dir.join(format!("store/f{i}")), "").unwrap();
    }
    symlink("store", dir.join("vendor")).unwrap();
}

/// Under a symlinked directory `realpath` never matches the path asked,
/// so a lookup reads the listings on its way. Each lookup read them all
/// again: `grep -r` over N files read N listings of N names, and over
/// 8,000 files took 26 s where 1 s would do.
#[tokio::test]
async fn lookups_under_a_symlink_read_each_listing_once_until_it_changes() {
    let t = tempfile::tempdir().unwrap();
    let dir = t.path();
    linked_store(dir, 100);
    // A disk with an exact clock: what was just made is kept at once.
    let s = Spelling {
        settled: Duration::ZERO,
        ..Spelling::of(dir)
    };
    for i in 0..100 {
        let asked = format!("/vendor/f{i}");
        assert_eq!(s.spelled(Path::new(&asked)).await, Spelled::AsAsked);
    }
    // The project's and the store's.
    assert_eq!(s.reads(), 2);
    // A change in the store is seen: its listing is read again. (Past a
    // coarse clock's tick first, so the change moves the stamp.)
    std::thread::sleep(Duration::from_millis(50));
    fs::rename(dir.join("store/f0"), dir.join("store/F0")).unwrap();
    for asked in ["/vendor/F0", "/vendor/f1"] {
        assert_eq!(
            s.spelled(Path::new(asked)).await,
            Spelled::AsAsked,
            "{asked}"
        );
    }
    assert_eq!(s.reads(), 3);
}

/// Where `realpath`'s answer is not the disk's word (exFAT, FAT, vfat),
/// every lookup asks the listings, and every one under another spelling
/// finds the disk's in them: each is still read once, not once a lookup.
#[tokio::test]
async fn lookups_the_listings_decide_read_each_listing_once() {
    let t = tempfile::tempdir().unwrap();
    let dir = t.path();
    linked_store(dir, 100);
    let s = Spelling {
        settled: Duration::ZERO,
        ..echoing(dir)
    };
    for i in 0..100 {
        let asked = format!("/store/f{i}");
        assert_eq!(s.spelled(Path::new(&asked)).await, Spelled::AsAsked);
        let other = format!("/store/F{i}");
        let want = if dir.join(&other[1..]).exists() {
            Spelled::Other(Some(PathBuf::from(&asked)))
        } else {
            Spelled::AsAsked
        };
        assert_eq!(s.spelled(Path::new(&other)).await, want, "{other}");
    }
    // The project's and the store's.
    assert_eq!(s.reads(), 2);
}

/// Where `realpath` gives back each name as the directory lists it (APFS
/// and HFS+ answer with the stored name; a disk that tells cases apart
/// finds no other), a lookup as asked reads no listing at all.
#[tokio::test]
async fn lookups_realpath_answers_read_no_listing() {
    let t = tempfile::tempdir().unwrap();
    let dir = t.path();
    linked_store(dir, 100);
    let s = Spelling {
        settled: Duration::ZERO,
        ..Spelling::of(dir)
    };
    for i in 0..100 {
        let asked = format!("/store/f{i}");
        assert_eq!(s.spelled(Path::new(&asked)).await, Spelled::AsAsked);
    }
    assert_eq!(s.reads(), 0);
}

/// A directory changed within [`SETTLED`] of its listing's read may
/// change again with the same stamp: its listing is read at every
/// lookup until it settles.
#[tokio::test]
async fn a_listing_is_kept_only_once_its_directory_settled() {
    let t = tempfile::tempdir().unwrap();
    let dir = t.path();
    linked_store(dir, 3);
    let s = Spelling::of(dir);
    for i in 0..3 {
        let asked = format!("/vendor/f{i}");
        assert_eq!(s.spelled(Path::new(&asked)).await, Spelled::AsAsked);
    }
    assert_eq!(s.reads(), 6);
}

/// The root of an exFAT, FAT or vfat volume never changes its ctime, so
/// its first listing was kept for good: a file the human made there later
/// read as absent, Write said it created it, and applying went over the
/// human's. Listed again at each lookup instead, a walk over N files at
/// such a root read N listings of N names: `grep -r` over 2,000 files at
/// a stick's root took minutes. The root's listing is kept, but answers
/// only for the names it holds; a name it lacks reads it again.
#[tokio::test]
async fn a_mount_roots_listing_answers_only_for_the_names_it_holds() {
    let t = tempfile::tempdir().unwrap();
    let dir = t.path();
    fs::write(dir.join("a"), "").unwrap();
    let mut s = Spelling {
        settled: Duration::ZERO,
        ..echoing(dir)
    };
    s.answers.mount_root = |_, _| true;
    for _ in 0..3 {
        assert_eq!(s.spelled(Path::new("/a")).await, Spelled::AsAsked);
    }
    assert_eq!(s.reads(), 1, "the root's listing, read once");
    fs::write(dir.join("b"), "the human's\n").unwrap();
    // Such a root keeps its stamp whatever is added there.
    s.listings
        .restamp(dir, Stamp::of(&fs::metadata(dir).unwrap()));
    assert_eq!(s.spelled(Path::new("/b")).await, Spelled::AsAsked);
    assert_eq!(s.reads(), 2, "read again for the name it lacked");
}

/// What tells the root of a mount: its `..` is on another device, or is
/// itself.
#[test]
fn the_root_of_a_mount_is_told_by_its_parent() {
    let mount_root = |dir: &Path| (Answers::SYSTEM.mount_root)(dir, &fs::metadata(dir).unwrap());
    assert!(mount_root(Path::new("/")));
    let t = tempfile::tempdir().unwrap();
    fs::create_dir(t.path().join("d")).unwrap();
    assert!(!mount_root(&t.path().join("d")));
}

/// A symlink that dangles or loops has no `realpath`; it is found by its
/// directory's listing, and its place by the entry that is the same link.
#[tokio::test]
async fn a_link_that_does_not_resolve_is_found_only_as_the_disk_spells_it() {
    let t = tempfile::tempdir().unwrap();
    let dir = t.path();
    fs::create_dir(dir.join("lib")).unwrap();
    symlink("nowhere", dir.join("lib/d")).unwrap();
    symlink("l2", dir.join("lib/l1")).unwrap();
    symlink("l1", dir.join("lib/l2")).unwrap();
    let s = Spelling::of(dir);
    for asked in ["/lib/d", "/lib/l1", "/lib/l2"] {
        assert_eq!(
            s.spelled(Path::new(asked)).await,
            Spelled::AsAsked,
            "{asked}"
        );
    }
    for (asked, disk) in [
        ("/LIB/d", "/lib/d"),
        ("/lib/D", "/lib/d"),
        ("/lib/L1", "/lib/l1"),
    ] {
        let folds = fs::symlink_metadata(dir.join(&asked[1..])).is_ok();
        let want = if folds {
            Spelled::Other(Some(PathBuf::from(disk)))
        } else {
            Spelled::AsAsked
        };
        assert_eq!(s.spelled(Path::new(asked)).await, want, "{asked}");
    }
}

/// How the disk spells the way to `asked`, asked afresh.
async fn way(s: &Spelling, asked: &str) -> Spelled {
    s.spelled_way(Path::new(asked), &mut Ways::default()).await
}

/// A path that is not there lands in the nearest directory above it that
/// is, as the disk spells that one.
#[tokio::test]
async fn the_way_to_a_missing_path_is_its_nearest_directory_there() {
    let t = tempfile::tempdir().unwrap();
    let dir = t.path();
    fs::create_dir(dir.join("lib")).unwrap();
    let s = Spelling::of(dir);
    for asked in ["/lib/new.rs", "/lib/new/deeper/x.rs", "/nope/x.rs", "/x.rs"] {
        assert_eq!(way(&s, asked).await, Spelled::AsAsked, "{asked}");
    }
    let want = if dir.join("LIB").exists() {
        Spelled::Other(Some(PathBuf::from("/lib")))
    } else {
        Spelled::AsAsked
    };
    for asked in ["/LIB/new.rs", "/LIB/new/deeper/x.rs"] {
        assert_eq!(way(&s, asked).await, want, "{asked}");
    }
}

/// Below a file the disk spells another way (`/NOTES/x/y` beside the
/// file `notes`), the disk answers "not a directory" for `NOTES/x`: the
/// way goes on up to the file, and it ended there as asked.
#[tokio::test]
async fn the_way_below_a_file_spelled_another_way_is_that_file() {
    let t = tempfile::tempdir().unwrap();
    let dir = t.path();
    fs::write(dir.join("notes"), "").unwrap();
    let s = Spelling::of(dir);
    let want = if dir.join("NOTES").exists() {
        Spelled::Other(Some(PathBuf::from("/notes")))
    } else {
        Spelled::AsAsked
    };
    assert_eq!(way(&s, "/NOTES/x/y").await, want);
    assert_eq!(way(&s, "/notes/x/y").await, Spelled::AsAsked);
}

/// Each new file asked its way up through the same new directories
/// again: one scan of a new tree asked each of them once per file.
#[tokio::test]
async fn the_files_of_one_new_tree_ask_their_way_once() {
    let t = tempfile::tempdir().unwrap();
    let dir = t.path();
    fs::create_dir(dir.join("lib")).unwrap();
    let s = Spelling::of(dir);
    let mut ways = Ways::default();
    for i in 0..20 {
        let asked = format!("/lib/new/deeper/f{i}.rs");
        let got = s.spelled_way(Path::new(&asked), &mut ways).await;
        assert_eq!(got, Spelled::AsAsked, "{asked}");
    }
    // `/lib/new/deeper`, `/lib/new` and `/lib`.
    assert_eq!(ways.0.len(), 3, "{ways:?}");
}

#[tokio::test]
async fn a_directory_that_does_not_resolve_reads_every_path_as_asked() {
    let s = Spelling::of(Path::new("/no/such/dir"));
    assert_eq!(s.spelled(Path::new("/LIB")).await, Spelled::AsAsked);
}
