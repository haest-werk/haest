//! A move or a copy to another spelling of a name the disk holds, where the
//! old spelling stays in the sandbox (made again, or not emptied). On a disk
//! that reads both spellings as one name, the sandbox's end state cannot
//! exist: every write under the new spelling would land on the old one, so it
//! is a conflict. What the sandbox did to the old spelling waits with it, so
//! the disk never loses a file the sandbox could not write elsewhere. On a
//! disk that tells the spellings apart, everything applies.

mod common;

use std::fs;

use common::{
    sandbox,
    spelling::{apply_all, changes, contents, pairs, paths, reads_as_one, run},
    tempdir,
};
use haest_core::workspace::Change;

/// The disk's `lib` holds `a.rs` and `b.rs`. The deletes and overwrites
/// under `lib` were applied while every write under `Lib` was skipped as a
/// conflict, so `lib`'s files were then only in the sandbox, lost when it
/// ended.
#[tokio::test]
async fn a_move_to_another_spelling_that_cannot_land_takes_nothing_from_the_disk() {
    /// The command, the files a disk that reads `lib` and `Lib` as one name
    /// holds after the apply, and those a disk that tells them apart holds.
    struct Case {
        command: &'static str,
        one_name: &'static [(&'static str, &'static str)],
        two_names: &'static [(&'static str, &'static str)],
    }
    const KEPT: &[(&str, &str)] = &[("lib/a.rs", "a\n"), ("lib/b.rs", "b\n")];
    const MOVED: &[(&str, &str)] = &[("Lib/a.rs", "a\n"), ("Lib/b.rs", "b\n")];
    let cases = [
        Case {
            command: "mv lib Lib && mkdir lib",
            one_name: KEPT,
            two_names: MOVED,
        },
        Case {
            command: "mkdir Lib && mv lib/* Lib/",
            one_name: KEPT,
            two_names: MOVED,
        },
        Case {
            command: "mkdir Lib && mv lib/a.rs Lib/",
            one_name: KEPT,
            two_names: &[("Lib/a.rs", "a\n"), ("lib/b.rs", "b\n")],
        },
        Case {
            command: "cp -r lib Lib && rm lib/a.rs",
            one_name: KEPT,
            two_names: &[
                ("Lib/a.rs", "a\n"),
                ("Lib/b.rs", "b\n"),
                ("lib/b.rs", "b\n"),
            ],
        },
        Case {
            command: "mv lib Lib && echo x > lib",
            one_name: KEPT,
            two_names: &[("Lib/a.rs", "a\n"), ("Lib/b.rs", "b\n"), ("lib", "x\n")],
        },
        Case {
            command: "mv lib Lib && mkdir lib && echo new > lib/a.rs",
            one_name: KEPT,
            two_names: &[
                ("Lib/a.rs", "a\n"),
                ("Lib/b.rs", "b\n"),
                ("lib/a.rs", "new\n"),
            ],
        },
        // A file added under the old spelling takes nothing away: it lands.
        Case {
            command: "mv lib Lib && mkdir lib && echo c > lib/c.rs",
            one_name: &[
                ("lib/a.rs", "a\n"),
                ("lib/b.rs", "b\n"),
                ("lib/c.rs", "c\n"),
            ],
            two_names: &[
                ("Lib/a.rs", "a\n"),
                ("Lib/b.rs", "b\n"),
                ("lib/c.rs", "c\n"),
            ],
        },
    ];
    for Case {
        command,
        one_name,
        two_names,
    } in cases
    {
        let (_t, dir) = tempdir();
        fs::create_dir(dir.join("lib")).unwrap();
        fs::write(dir.join("lib/a.rs"), "a\n").unwrap();
        fs::write(dir.join("lib/b.rs"), "b\n").unwrap();
        let one = reads_as_one(&dir, "lib", "Lib");
        let ws = sandbox(&dir).await;
        run(&ws, &dir, command).await;
        if !one {
            apply_all(&ws).await;
            assert_eq!(contents(&dir), pairs(two_names), "{command}");
            continue;
        }
        let report = changes(&ws).apply().await;
        assert_eq!(contents(&dir), pairs(one_name), "{command}: {report:?}");
        assert!(report.failed.is_empty(), "{command}: {report:?}");
        // What waits is a conflict, and stays listed until the human settles it.
        let left = changes(&ws).list().await;
        assert_eq!(paths(&left), paths(&report.skipped), "{command}");
        assert!(left.iter().all(|c| c.conflict), "{command}: {left:?}");
    }
}

/// The same between Unicode normalizations, which APFS reads as one name on
/// a case-sensitive volume too: `mv café cafe\u{301} && mkdir café` deleted
/// `menu.md` and wrote nothing.
#[tokio::test]
async fn a_move_to_another_normalization_that_cannot_land_takes_nothing_from_the_disk() {
    let (_t, dir) = tempdir();
    fs::create_dir(dir.join("caf\u{e9}")).unwrap();
    fs::write(dir.join("caf\u{e9}/menu.md"), "menu\n").unwrap();
    let one = reads_as_one(&dir, "caf\u{e9}", "cafe\u{301}");
    let ws = sandbox(&dir).await;
    run(&ws, &dir, "mv caf\u{e9} cafe\u{301} && mkdir caf\u{e9}").await;
    let report = changes(&ws).apply().await;
    assert!(report.failed.is_empty(), "{report:?}");
    let want = if one {
        ("caf\u{e9}/menu.md", "menu\n")
    } else {
        ("cafe\u{301}/menu.md", "menu\n")
    };
    assert_eq!(
        contents(&dir),
        pairs(&[want]),
        "one name: {one}: {report:?}"
    );
}

/// Once the human settles the old spelling (the model removes the `lib` it
/// made again), the rename applies whole.
#[tokio::test]
async fn a_held_rename_applies_once_the_old_spelling_goes() {
    let (_t, dir) = tempdir();
    fs::create_dir(dir.join("lib")).unwrap();
    fs::write(dir.join("lib/a.rs"), "a\n").unwrap();
    let ws = sandbox(&dir).await;
    run(&ws, &dir, "mv lib Lib && mkdir lib").await;
    let _ = changes(&ws).apply().await;
    run(&ws, &dir, "rmdir lib").await;
    apply_all(&ws).await;
    assert_eq!(contents(&dir), pairs(&[("Lib/a.rs", "a\n")]));
}

/// The rename waits (`mv lib Lib && mkdir lib`), the human saves
/// `lib/a.rs`, and the sandbox removes the `lib` it made again. That delete
/// freed the name, so the apply removed `lib` with the human's save in it
/// and wrote `Lib`'s old copy. (A disk that tells the spellings apart
/// applies the rename at once: above.)
#[tokio::test]
async fn a_held_rename_keeps_a_save_the_human_makes_under_the_old_spelling() {
    let (_t, dir) = tempdir();
    fs::create_dir(dir.join("lib")).unwrap();
    fs::write(dir.join("lib/a.rs"), "a\n").unwrap();
    fs::write(dir.join("lib/b.rs"), "b\n").unwrap();
    if !reads_as_one(&dir, "lib", "Lib") {
        return;
    }
    let ws = sandbox(&dir).await;
    run(&ws, &dir, "mv lib Lib && mkdir lib").await;
    let _ = changes(&ws).apply().await;
    fs::write(dir.join("lib/a.rs"), "human\n").unwrap();
    run(&ws, &dir, "rmdir lib").await;
    let report = changes(&ws).apply().await;
    let want = [("lib/a.rs", "human\n"), ("lib/b.rs", "b\n")];
    assert_eq!(contents(&dir), pairs(&want), "{report:?}");
    assert!(report.applied.is_empty(), "{report:?}");
    assert!(report.failed.is_empty(), "{report:?}");
    let left = changes(&ws).list().await;
    assert_eq!(paths(&left), paths(&report.skipped));
    assert!(left.iter().all(|c| c.conflict), "{left:?}");
}

/// A rename by case whose delete would stop partway (a read-only directory
/// inside the old name): `remove_all` took `a.rs` before it failed, and
/// every write under `Lib` was then skipped, so `a.rs` was only in the
/// sandbox. Every file is on the disk after the first apply, under its old
/// name or its new one, and the second, once the human made the directory
/// writable, finishes the rename. (As root nothing is read-only, and the
/// rename applies at once.)
#[tokio::test]
async fn a_rename_by_case_whose_delete_would_stop_partway_keeps_every_file() {
    use std::os::unix::fs::PermissionsExt;
    let (_t, dir) = tempdir();
    fs::create_dir_all(dir.join("lib/ro")).unwrap();
    fs::write(dir.join("lib/a.rs"), "a\n").unwrap();
    fs::write(dir.join("lib/ro/x.txt"), "x\n").unwrap();
    let mode = |m| fs::set_permissions(dir.join("lib/ro"), fs::Permissions::from_mode(m));
    mode(0o555).unwrap();
    let ws = sandbox(&dir).await;
    run(&ws, &dir, "mv lib Lib").await;
    let first = changes(&ws).apply().await;
    let after_first = contents(&dir);
    let _ = mode(0o755);
    for (file, body) in [("a.rs", "a\n"), ("ro/x.txt", "x\n")] {
        let held = |name: &str| {
            let path = format!("{name}/{file}");
            after_first.iter().any(|(p, b)| *p == path && b == body)
        };
        assert!(
            held("lib") || held("Lib"),
            "{file} is on the disk: {after_first:?}, {first:?}"
        );
    }
    let second = changes(&ws).apply().await;
    // `ro` keeps its mode through the rename, as with `mv`; writable again,
    // so the temp dir goes.
    let ro = dir.join("Lib/ro");
    let kept = fs::metadata(&ro).map(|m| m.permissions().mode() & 0o7777);
    let _ = fs::set_permissions(&ro, fs::Permissions::from_mode(0o755));
    assert_eq!(kept.ok(), Some(0o555), "{first:?}, {second:?}");
    let want = [("Lib/a.rs", "a\n"), ("Lib/ro/x.txt", "x\n")];
    assert_eq!(contents(&dir), pairs(&want), "{first:?}, {second:?}");
    assert_eq!(changes(&ws).list().await, Vec::<Change>::new());
}
