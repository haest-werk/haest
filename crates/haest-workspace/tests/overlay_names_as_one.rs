//! Applying on a disk that reads two names of a file as one (macOS's
//! default: case and Unicode normalization; any APFS: normalization). The
//! sandbox reads the disk through the system, so `cat README.md` finds the
//! disk's `readme.md`, as bash does; its own files tell every name apart,
//! so after `mv README.md readme.md` it may show one file on the disk as
//! two. Every test holds on a disk that tells the names apart too, and says
//! what each kind of disk ends up holding.
//!
//! Where the disk reads two names as one, a change that would take away
//! what the sandbox shows under the other name, or land under it, is held
//! as a conflict: skipped, and the disk keeps the human's files as they
//! were. Nothing the sandbox shows is lost (`nothing_lost`).

mod common;

use std::{fs, path::Path};

use common::{
    disk::{
        DIRECTORY, Disk, NUMBERED, RENAME_AND_COPY, RENAME_THEN_EDIT, RENAME_TO_LOWER,
        RENAME_TO_LOWER_THEN_EDIT, RENAME_WITH_NO_ASCII_LETTER, copy_by_case_then_edit,
    },
    opts,
    project::{
        apply_all, changes, contents, folds_case, nothing_lost, pairs, paths, reads_as_one, run,
        writable_again,
    },
    sandbox, tempdir,
};
use haest_core::workspace::{ApplyFailureCode, ChangeKind, Workspace};

const HUMAN: &str = "the human's\n";

/// What the files on the disk under `dir` hold, whatever their names.
fn texts(dir: &Path) -> Vec<String> {
    contents(dir).into_iter().map(|(_, text)| text).collect()
}

/// Where the disk reads both names as one, the new name read the same as the
/// disk and was never listed: only the old name's delete was, and applying it
/// deleted the file. On a disk that tells them apart it is a plain rename.
/// (`overlay_folding_disk.rs` runs these renames on exFAT, FAT and vfat.)
#[tokio::test]
async fn a_rename_by_case_alone_loses_nothing() {
    for case in [
        RENAME_AND_COPY,
        RENAME_TO_LOWER,
        RENAME_WITH_NO_ASCII_LETTER,
        RENAME_THEN_EDIT,
        RENAME_TO_LOWER_THEN_EDIT,
        DIRECTORY,
        NUMBERED,
    ] {
        case.check(&Disk::temp()).await;
    }
}

/// A copy by case lands on the human's file where the disk reads both names
/// as one: a conflict, never written over it.
#[tokio::test]
async fn a_copy_by_case_never_goes_over_the_humans_file() {
    copy_by_case_then_edit(&Disk::temp()).await;
}

/// Renames from the human's name to one the disk may read as the same:
/// by case, and from Unicode's decomposed form to its composed one, with
/// no ASCII letter and with one.
const RENAMES: [(&str, &str); 3] = [
    ("README.md", "readme.md"),
    ("\u{1112}\u{1161}\u{11ab}", "\u{d55c}"),
    ("cafe\u{301}.txt", "caf\u{e9}.txt"),
];

/// The new name reads as the human's file, so the rename lists only the
/// old name as deleted. Applying deleted it, the one file there was, and
/// the sandbox still showed the new name, its only copy in memory. An
/// empty file too: on exFAT and FAT it has no inode number of its own on
/// the disk, only one per `lstat` the system makes up, the same under both
/// names.
#[tokio::test]
async fn a_rename_the_disk_reads_as_one_name_deletes_nothing() {
    for text in [HUMAN, ""] {
        for (from, to) in RENAMES {
            let (_t, dir) = tempdir();
            fs::write(dir.join(from), text).unwrap();
            let one = reads_as_one(&dir, from, to);
            let ws = sandbox(&dir).await;
            run(&ws, &dir, &format!("mv '{from}' '{to}'")).await;
            let report = changes(&ws).apply().await;
            nothing_lost(&ws, &dir, &report).await;
            assert_eq!(texts(&dir), [text], "{from} -> {to}: {report:?}");
            let shown = ws.fs().read(&dir.join(to)).await.unwrap();
            assert_eq!(shown, text.as_bytes(), "{from} -> {to}");
            if one {
                assert_eq!(paths(&report.skipped), [format!("/{from}")]);
            } else {
                assert!(report.skipped.is_empty(), "{report:?}");
            }
        }
    }
}

/// `rm readme.md` where the disk holds `README.md`: the system finds the
/// one by the other, as bash does, and the delete would take the human's
/// file under a name the sandbox did not remove. (A disk that tells the
/// names apart has no `readme.md` to remove.)
#[tokio::test]
async fn a_delete_of_a_name_the_disk_holds_under_another_never_takes_that_one() {
    let (_t, dir) = tempdir();
    fs::write(dir.join("README.md"), HUMAN).unwrap();
    if !reads_as_one(&dir, "README.md", "readme.md") {
        return;
    }
    let ws = sandbox(&dir).await;
    run(&ws, &dir, "rm readme.md").await;
    let report = changes(&ws).apply().await;
    nothing_lost(&ws, &dir, &report).await;
    assert!(report.applied.is_empty(), "{report:?}");
    assert_eq!(contents(&dir), pairs(&[("README.md", HUMAN)]));
}

/// The model copies `README.md` to `readme.md`, one file on the disk, and
/// rewrites `README.md`: the rewrite went over the copy's text, the human's,
/// which then was only in the sandbox.
#[tokio::test]
async fn an_overwrite_never_takes_a_file_the_sandbox_shows_under_another_name() {
    let (_t, dir) = tempdir();
    fs::write(dir.join("README.md"), HUMAN).unwrap();
    let one = reads_as_one(&dir, "README.md", "readme.md");
    let ws = sandbox(&dir).await;
    run(&ws, &dir, "cp README.md readme.md && echo new > README.md").await;
    let report = changes(&ws).apply().await;
    nothing_lost(&ws, &dir, &report).await;
    if one {
        assert!(report.applied.is_empty(), "{report:?}");
        assert_eq!(contents(&dir), pairs(&[("README.md", HUMAN)]));
    } else {
        let want = [("README.md", "new\n"), ("readme.md", HUMAN)];
        assert_eq!(contents(&dir), pairs(&want), "{report:?}");
    }
}

/// The model writes `notes.md`, and the human then saves `NOTES.md`. Where
/// the disk reads both as one, the model's write would go over the human's
/// file: it is a conflict. Where it tells them apart, both stay.
#[tokio::test]
async fn a_file_the_human_saves_under_another_name_after_the_models_write_stays() {
    let (_t, dir) = tempdir();
    let one = folds_case(&dir);
    let ws = sandbox(&dir).await;
    run(&ws, &dir, "echo draft > notes.md").await;
    fs::write(dir.join("NOTES.md"), HUMAN).unwrap();
    let report = changes(&ws).apply().await;
    assert!(report.failed.is_empty(), "{report:?}");
    if one {
        assert_eq!(paths(&report.skipped), ["/notes.md"], "{report:?}");
        assert_eq!(contents(&dir), pairs(&[("NOTES.md", HUMAN)]));
        let left = changes(&ws).list().await;
        assert!(left.iter().all(|c| c.conflict), "{left:?}");
    } else {
        let want = [("NOTES.md", HUMAN), ("notes.md", "draft\n")];
        assert_eq!(contents(&dir), pairs(&want), "{report:?}");
    }
}

/// `caf\u{e9}` (one code point) and `cafe\u{301}` (e and a combining accent)
/// are one name on a disk that ignores Unicode normalization (any APFS, a
/// case-sensitive one too): a rename between them deleted the file, or
/// moved a directory into itself.
#[tokio::test]
async fn a_rename_between_unicode_normalizations_loses_nothing() {
    for (files, command, want, names) in [
        (
            &[("caf\u{e9}.md", "menu\n")][..],
            "mv caf\u{e9}.md cafe\u{301}.md",
            &[("cafe\u{301}.md", "menu\n")][..],
            ("caf\u{e9}.md", "cafe\u{301}.md"),
        ),
        (
            &[("caf\u{e9}/menu.md", "menu\n")][..],
            "mv caf\u{e9} cafe\u{301}",
            &[("cafe\u{301}/menu.md", "menu\n")][..],
            ("caf\u{e9}", "cafe\u{301}"),
        ),
    ] {
        let disk = Disk::temp();
        disk.make(files);
        let one = reads_as_one(&disk.dir, names.0, names.1);
        let ws = disk.sandbox().await;
        run(&ws, &disk.dir, command).await;
        if !one {
            apply_all(&ws).await;
            assert_eq!(contents(&disk.dir), pairs(want), "{command}");
            continue;
        }
        let report = changes(&ws).apply().await;
        nothing_lost(&ws, &disk.dir, &report).await;
        assert!(report.applied.is_empty(), "{command}: {report:?}");
        assert_eq!(contents(&disk.dir), pairs(files), "{command}");
    }
}

/// The human edits the file after the sandbox renamed it. Where the disk
/// reads both names as one, both changes are conflicts and the edit stays;
/// where it does not, the new file is added and the edited one stays.
#[tokio::test]
async fn a_human_edit_after_a_case_rename_stays_a_conflict() {
    let (_t, dir) = tempdir();
    fs::write(dir.join("readme.md"), "v1\n").unwrap();
    let one = reads_as_one(&dir, "readme.md", "README.md");
    let ws = sandbox(&dir).await;
    run(&ws, &dir, "mv readme.md README.md").await;
    fs::write(dir.join("readme.md"), "the human was here\n").unwrap();
    let report = changes(&ws).apply().await;
    nothing_lost(&ws, &dir, &report).await;
    let (want_skipped, want_disk): (&[&str], &[(&str, &str)]) = if one {
        (
            &["/README.md", "/readme.md"],
            &[("readme.md", "the human was here\n")],
        )
    } else {
        (
            &["/readme.md"],
            &[("README.md", "v1\n"), ("readme.md", "the human was here\n")],
        )
    };
    assert_eq!(
        paths(&report.skipped),
        want_skipped,
        "one name: {one}: {report:?}"
    );
    assert_eq!(contents(&dir), pairs(want_disk), "one name: {one}");
}

/// The sandbox writes `NOTES.md` and removes `notes.md`, with a human edit
/// to `notes.md` before that `rm` or after it. Where the disk reads both
/// names as one, `NOTES.md` is the human's file read through, so both
/// changes wait as conflicts and the human's edit stays. Where it tells
/// them apart, an edit made before the `rm` is one the sandbox saw and
/// deleted: both apply. One made after is a conflict, and stays one.
#[tokio::test]
async fn a_conflict_stays_a_conflict_after_a_delete_under_another_name() {
    for edit_first in [true, false] {
        let (_t, dir) = tempdir();
        fs::write(dir.join("notes.md"), "v1\n").unwrap();
        let one = reads_as_one(&dir, "notes.md", "NOTES.md");
        let ws = sandbox(&dir).await;
        run(&ws, &dir, "echo draft > NOTES.md").await;
        if edit_first {
            fs::write(dir.join("notes.md"), HUMAN).unwrap();
        }
        run(&ws, &dir, "rm notes.md").await;
        if !edit_first {
            fs::write(dir.join("notes.md"), HUMAN).unwrap();
        }
        let report = changes(&ws).apply().await;
        let case = format!("edit first: {edit_first}, one name: {one}: {report:?}");
        nothing_lost(&ws, &dir, &report).await;
        let (applied, skipped) = (paths(&report.applied), paths(&report.skipped));
        let disk: &[(&str, &str)] = if one {
            assert!(applied.is_empty(), "{case}");
            assert_eq!(skipped, ["/NOTES.md", "/notes.md"], "{case}");
            &[("notes.md", HUMAN)]
        } else if edit_first {
            assert_eq!(applied, ["/NOTES.md", "/notes.md"], "{case}");
            &[("NOTES.md", "draft\n")]
        } else {
            assert_eq!(applied, ["/NOTES.md"], "{case}");
            assert_eq!(skipped, ["/notes.md"], "{case}");
            &[("NOTES.md", "draft\n"), ("notes.md", HUMAN)]
        };
        assert_eq!(contents(&dir), pairs(disk), "{case}");
    }
}

/// The sandbox writes two names, neither on the disk before. Where the
/// disk reads them as one file, the first written (upper case sorts first)
/// made the second's real path move: the second was written over it all
/// the same, both were reported applied, and the sandbox's `README.md` was
/// lost, listed nowhere. It is skipped as a conflict and stays listed.
/// Where the disk tells them apart, both are applied.
#[tokio::test]
async fn two_names_the_sandbox_wrote_never_overwrite_each_other() {
    let (_t, dir) = tempdir();
    let one = folds_case(&dir);
    let ws = sandbox(&dir).await;
    let command = "echo upper > README.md && echo lower > readme.md";
    run(&ws, &dir, command).await;
    let report = changes(&ws).apply().await;
    nothing_lost(&ws, &dir, &report).await;
    let left = changes(&ws).list().await;
    if one {
        assert_eq!(paths(&report.applied), ["/README.md"], "{report:?}");
        assert_eq!(paths(&report.skipped), ["/readme.md"], "{report:?}");
        assert_eq!(contents(&dir), pairs(&[("README.md", "upper\n")]));
        assert_eq!(paths(&left), ["/readme.md"], "{report:?}");
    } else {
        let both = ["/README.md", "/readme.md"];
        assert_eq!(paths(&report.applied), both, "{report:?}");
        let want = [("README.md", "upper\n"), ("readme.md", "lower\n")];
        assert_eq!(contents(&dir), pairs(&want));
        assert!(left.is_empty(), "{left:?}");
    }
}

/// The same with two names of a directory. Where the disk reads them as
/// one, it holds one directory: the name written first is applied whole,
/// and what the other holds would land in that one, so it waits as a
/// conflict. Its files went into the first directory and were reported
/// applied, so nothing listed them any more: removing the first name in
/// the sandbox later deleted them on apply, while the sandbox still showed
/// them under the second. Where the disk tells the names apart, both
/// directories are applied whole, and the disk ends as the sandbox shows it.
#[tokio::test]
async fn two_names_of_a_directory_the_sandbox_made_lose_nothing() {
    let (_t, dir) = tempdir();
    let one = folds_case(&dir);
    let ws = sandbox(&dir).await;
    run(
        &ws,
        &dir,
        "mkdir Lib lib && echo a > Lib/a.rs && echo b > lib/b.rs \
         && echo upper > Lib/same.rs && echo lower > lib/same.rs",
    )
    .await;
    let report = changes(&ws).apply().await;
    nothing_lost(&ws, &dir, &report).await;
    if one {
        let applied = ["/Lib", "/Lib/a.rs", "/Lib/same.rs"];
        assert_eq!(paths(&report.applied), applied, "{report:?}");
        let want = [("Lib/a.rs", "a\n"), ("Lib/same.rs", "upper\n")];
        assert_eq!(contents(&dir), pairs(&want));
    } else {
        assert_eq!(report.applied.len(), 6, "{report:?}");
        assert!(report.skipped.is_empty(), "{report:?}");
    }
    // The model tidies the first name away.
    run(&ws, &dir, "rm -r Lib").await;
    let report = changes(&ws).apply().await;
    nothing_lost(&ws, &dir, &report).await;
    if one {
        assert!(report.applied.is_empty(), "{report:?}");
        let want = [("Lib/a.rs", "a\n"), ("Lib/same.rs", "upper\n")];
        assert_eq!(contents(&dir), pairs(&want));
    } else {
        assert!(report.skipped.is_empty(), "{report:?}");
        let want = [("lib/b.rs", "b\n"), ("lib/same.rs", "lower\n")];
        assert_eq!(contents(&dir), pairs(&want));
    }
}

/// A name the sandbox writes under another name of a file or a directory
/// the disk holds, then the old name removed. The write landed on the
/// disk's name, was reported applied and left the change list, and the
/// sandbox kept showing the two apart: removing the old name in a later
/// round deleted both on apply. Where the disk reads the names as one,
/// some of these commands fail as they do in bash (`mkdir Lib` beside
/// `lib`: "File exists"); what is left waits as a conflict, and the human's
/// file stays. (The last case reads as one name on any APFS.)
#[tokio::test]
async fn a_write_under_another_name_of_what_the_disk_holds_is_never_forgotten() {
    /// The disk's one file (`x\n`), the sandbox's two rounds, and the one
    /// file a disk that tells the names apart holds after both.
    struct Case {
        file: &'static str,
        first: &'static str,
        second: &'static str,
        want: (&'static str, &'static str),
    }
    let cases = [
        Case {
            file: "lib/a.rs",
            first: "mkdir Lib && echo n > Lib/new.rs",
            second: "rm -r lib",
            want: ("Lib/new.rs", "n\n"),
        },
        Case {
            file: "lib/a.rs",
            first: "mkdir -p Lib/sub && echo n > Lib/sub/new.rs",
            second: "rm -r lib",
            want: ("Lib/sub/new.rs", "n\n"),
        },
        Case {
            file: "lib/a.rs",
            first: "cp -r lib Lib",
            second: "rm -r lib",
            want: ("Lib/a.rs", "x\n"),
        },
        Case {
            file: "readme.md",
            first: "cp readme.md README.md",
            second: "rm readme.md",
            want: ("README.md", "x\n"),
        },
        Case {
            file: "caf\u{e9}/a.rs",
            first: "mkdir cafe\u{301} && echo n > cafe\u{301}/new.rs",
            second: "rm -r caf\u{e9}",
            want: ("cafe\u{301}/new.rs", "n\n"),
        },
    ];
    for Case {
        file,
        first,
        second,
        want,
    } in cases
    {
        let (_t, dir) = tempdir();
        fs::create_dir_all(dir.join(file).parent().unwrap()).unwrap();
        fs::write(dir.join(file), "x\n").unwrap();
        let top = file.split('/').next().unwrap();
        let other = want.0.split('/').next().unwrap();
        let one = reads_as_one(&dir, top, other);
        let ws = sandbox(&dir).await;
        if !one {
            run(&ws, &dir, first).await;
            let report = changes(&ws).apply().await;
            nothing_lost(&ws, &dir, &report).await;
            run(&ws, &dir, second).await;
            apply_all(&ws).await;
            assert_eq!(contents(&dir), pairs(&[want]), "{first}, then {second}");
            continue;
        }
        let _ = ws.shell(first, opts(&dir)).await;
        let report = changes(&ws).apply().await;
        nothing_lost(&ws, &dir, &report).await;
        let kept = fs::read_to_string(dir.join(file));
        assert_eq!(kept.ok().as_deref(), Some("x\n"), "{first}: {report:?}");
        // Where `first` failed, `second` removes what the sandbox shows:
        // gone, as in bash. Else the sandbox shows it under the new name.
        let _ = ws.shell(second, opts(&dir)).await;
        let report = changes(&ws).apply().await;
        nothing_lost(&ws, &dir, &report).await;
    }
}

/// A rename by case whose delete fails partway (a read-only directory inside
/// the old name). The new name's writes landed in the old directory, where
/// the disk reads both names as one, and were reported applied; only the
/// delete stayed listed, and applying again once the human made the
/// directory writable deleted every file. Where the disk reads both names
/// as one, the rename is held whole, both times. Where it tells them
/// apart, the second apply finishes it. (As root nothing is read-only, and
/// the rename applies at once there.)
#[tokio::test]
async fn a_rename_by_case_whose_delete_fails_keeps_every_file() {
    use std::os::unix::fs::PermissionsExt;
    let (_t, dir) = tempdir();
    fs::create_dir_all(dir.join("lib/ro")).unwrap();
    fs::write(dir.join("lib/a.rs"), "a\n").unwrap();
    fs::write(dir.join("lib/ro/x.txt"), "x\n").unwrap();
    let one = reads_as_one(&dir, "lib", "Lib");
    let mode = |m| fs::set_permissions(dir.join("lib/ro"), fs::Permissions::from_mode(m));
    mode(0o555).unwrap();
    let ws = sandbox(&dir).await;
    run(&ws, &dir, "mv lib Lib").await;
    let first = changes(&ws).apply().await;
    let _ = mode(0o755);
    let after_first = contents(&dir);
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
    if one {
        nothing_lost(&ws, &dir, &second).await;
        assert!(second.applied.is_empty(), "{first:?}, {second:?}");
        let want = [("lib/a.rs", "a\n"), ("lib/ro/x.txt", "x\n")];
        assert_eq!(contents(&dir), pairs(&want), "{first:?}, {second:?}");
        return;
    }
    // `ro` keeps its mode through the rename, as with `mv`.
    let kept = writable_again(&dir.join("Lib/ro"));
    assert_eq!(kept, Some(0o555), "{first:?}, {second:?}");
    let want = [("Lib/a.rs", "a\n"), ("Lib/ro/x.txt", "x\n")];
    assert_eq!(contents(&dir), pairs(&want), "{first:?}, {second:?}");
    assert!(changes(&ws).list().await.is_empty());
}

/// A move or a copy to another name of a directory the disk holds, where
/// the old name stays in the sandbox (made again, or not emptied). Where
/// the disk reads both names as one, every write under the new name would
/// land in the old one, and the sandbox shows the old one's files under
/// the new name: the deletes and overwrites under `lib` were applied while
/// every write under `Lib` was skipped, so `lib`'s files were then only in
/// the sandbox. Some of these fail in the sandbox as in bash (`mkdir Lib`
/// beside `lib`); the rest waits, and the disk keeps the human's files.
#[tokio::test]
async fn a_move_to_another_name_that_cannot_land_takes_nothing_from_the_disk() {
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
            command: "mv lib Lib && mkdir lib && rm -rf Lib/lib",
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
        // A file added under the old name takes nothing away: it lands.
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
        if !one {
            run(&ws, &dir, command).await;
            apply_all(&ws).await;
            assert_eq!(contents(&dir), pairs(two_names), "{command}");
            continue;
        }
        let _ = ws.shell(command, opts(&dir)).await;
        let report = changes(&ws).apply().await;
        nothing_lost(&ws, &dir, &report).await;
        assert_eq!(contents(&dir), pairs(one_name), "{command}: {report:?}");
    }
}

/// The rename waits (`mv lib Lib && mkdir lib`), the human saves
/// `lib/a.rs`, and the sandbox removes the `lib` it made again. That delete
/// freed the name, so the apply removed `lib` with the human's save in it.
/// (A disk that tells the names apart applies the rename at once: above.)
#[tokio::test]
async fn a_held_rename_keeps_a_save_the_human_makes_under_the_old_name() {
    let (_t, dir) = tempdir();
    fs::create_dir(dir.join("lib")).unwrap();
    fs::write(dir.join("lib/a.rs"), "a\n").unwrap();
    fs::write(dir.join("lib/b.rs"), "b\n").unwrap();
    if !reads_as_one(&dir, "lib", "Lib") {
        return;
    }
    let ws = sandbox(&dir).await;
    run(&ws, &dir, "mv lib Lib && mkdir lib").await;
    let first = changes(&ws).apply().await;
    nothing_lost(&ws, &dir, &first).await;
    fs::write(dir.join("lib/a.rs"), "human\n").unwrap();
    run(&ws, &dir, "rmdir lib").await;
    let report = changes(&ws).apply().await;
    nothing_lost(&ws, &dir, &report).await;
    let want = [("lib/a.rs", "human\n"), ("lib/b.rs", "b\n")];
    assert_eq!(contents(&dir), pairs(&want), "{report:?}");
    assert!(report.applied.is_empty(), "{report:?}");
}

/// The human reviews a rename as the sandbox shows it. Where the disk tells
/// the names apart, `mv readme.md README.md` lists the old name deleted and
/// the new one added. Where it reads both as one, the new name is the
/// human's file read through, unchanged, so only the old name's delete is
/// listed, as no conflict; the apply holds it.
#[tokio::test]
async fn a_rename_by_case_lists_what_the_sandbox_shows() {
    use ChangeKind::{Added, Deleted};
    let (_t, dir) = tempdir();
    fs::write(dir.join("readme.md"), "x\n").unwrap();
    let one = reads_as_one(&dir, "readme.md", "README.md");
    let ws = sandbox(&dir).await;
    run(&ws, &dir, "mv readme.md README.md").await;
    let listed = changes(&ws).list().await;
    let got: Vec<_> = listed.iter().map(|c| (c.path.as_str(), c.kind)).collect();
    let want: &[(&str, ChangeKind)] = if one {
        &[("/readme.md", Deleted)]
    } else {
        &[("/README.md", Added), ("/readme.md", Deleted)]
    };
    assert_eq!(got, want, "one name: {one}: {listed:?}");
    assert!(listed.iter().all(|c| !c.conflict), "{listed:?}");
}

/// The sandbox finds a project path as the system does, as bash does:
/// where the disk reads both names as one, `cat README.md` reads
/// `readme.md`, and `echo new > README.md` writes over what the disk holds
/// under the other name, so it is held as a conflict and `readme.md` stays.
/// Where it tells them apart, `README.md` is a new file.
#[tokio::test]
async fn the_sandbox_finds_a_path_as_the_system_does() {
    let (_t, dir) = tempdir();
    fs::write(dir.join("readme.md"), "old\n").unwrap();
    let one = reads_as_one(&dir, "readme.md", "README.md");
    let ws = sandbox(&dir).await;
    let r = ws.shell("cat README.md", opts(&dir)).await;
    if one {
        assert_eq!(r.stdout, "old\n", "{r:?}");
    } else {
        assert_eq!(r.exit_code, 1, "{r:?}");
    }
    run(&ws, &dir, "echo new > README.md").await;
    let listed = changes(&ws).list().await;
    let kind = if one {
        ChangeKind::Modified
    } else {
        ChangeKind::Added
    };
    let got: Vec<_> = listed.iter().map(|c| (c.path.as_str(), c.kind)).collect();
    assert_eq!(got, [("/README.md", kind)], "one name: {one}");
    let report = changes(&ws).apply().await;
    nothing_lost(&ws, &dir, &report).await;
    let mut want = vec![("readme.md", "old\n")];
    if !one {
        want.insert(0, ("README.md", "new\n"));
    }
    assert_eq!(contents(&dir), pairs(&want), "one name: {one}");
}

/// A tree made beside a file the disk reads under another name (`NOTES/x`
/// beside the file `notes`). Where the disk reads both names as one,
/// `mkdir -p NOTES/x` fails, as in bash ("Not a directory"), and nothing
/// changes. Where it tells them apart, it is a new tree.
#[tokio::test]
async fn a_tree_beside_a_file_the_disk_reads_as_its_name_fails_as_in_bash() {
    let (_t, dir) = tempdir();
    fs::write(dir.join("notes"), "n\n").unwrap();
    let one = reads_as_one(&dir, "notes", "NOTES");
    let ws = sandbox(&dir).await;
    let r = ws
        .shell("mkdir -p NOTES/x && echo y > NOTES/x/y", opts(&dir))
        .await;
    if one {
        assert_ne!(r.exit_code, 0, "{r:?}");
        assert!(changes(&ws).list().await.is_empty());
        return;
    }
    assert_eq!(r.exit_code, 0, "{r:?}");
    apply_all(&ws).await;
    let want = [("NOTES/x/y", "y\n"), ("notes", "n\n")];
    assert_eq!(contents(&dir), pairs(&want));
}

/// A symlink that does not resolve (dangling, or one of a loop) in a
/// directory renamed by case or Unicode form. Where the disk tells the
/// names apart, each link is listed and refused like any link the sandbox
/// made. Where it reads them as one, nothing is written or deleted, and
/// the disk keeps every link and file as it was.
#[tokio::test]
async fn a_link_that_does_not_resolve_in_a_renamed_directory_is_kept() {
    use std::os::unix::fs::symlink;
    for (old, new, command) in [
        ("lib", "Lib", "mv lib Lib"),
        ("lib", "Lib", "mv lib tmp && mv tmp Lib"),
        ("caf\u{e9}", "cafe\u{301}", "mv caf\u{e9} cafe\u{301}"),
    ] {
        let (_t, dir) = tempdir();
        fs::create_dir(dir.join(old)).unwrap();
        fs::write(dir.join(old).join("a.rs"), "a\n").unwrap();
        symlink("nowhere", dir.join(old).join("d")).unwrap();
        symlink("l2", dir.join(old).join("l1")).unwrap();
        symlink("l1", dir.join(old).join("l2")).unwrap();
        let one = reads_as_one(&dir, old, new);
        let ws = sandbox(&dir).await;
        run(&ws, &dir, command).await;
        let report = changes(&ws).apply().await;
        let refused = |code| {
            report
                .failed
                .iter()
                .filter(|f| f.code == code)
                .map(|f| f.change.path.clone())
                .collect::<Vec<_>>()
        };
        if one {
            assert!(report.applied.is_empty(), "{command}: {report:?}");
            assert!(report.skipped.iter().all(|c| c.conflict), "{report:?}");
            assert_eq!(
                refused(ApplyFailureCode::Symlink).len(),
                report.failed.len()
            );
            let at = |name: &str| dir.join(old).join(name);
            assert_eq!(fs::read_to_string(at("a.rs")).unwrap(), "a\n");
            for link in ["d", "l1", "l2"] {
                let meta = fs::symlink_metadata(at(link)).unwrap();
                assert!(meta.file_type().is_symlink(), "{command}: {link}");
            }
            continue;
        }
        let links = ["d", "l1", "l2"].map(|l| format!("/{new}/{l}"));
        let mut failed = refused(ApplyFailureCode::Symlink);
        failed.sort();
        assert_eq!(failed, links, "{command}: {report:?}");
        assert_eq!(report.failed.len(), 3, "{command}: {report:?}");
        assert!(report.skipped.is_empty(), "{command}: {report:?}");
    }
}
