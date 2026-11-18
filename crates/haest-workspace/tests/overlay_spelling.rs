//! Applying on a disk that reads some spellings of a name as one (macOS's
//! default: case and Unicode normalization). The sandbox's overlay tells
//! `readme.md` and `README.md` apart; under it the disk holds one file for
//! both. Every test holds on a disk that tells them apart too, and says what
//! each kind of disk ends up holding.
//!
//! What the sandbox shows of the project is what the disk holds once every
//! change is applied: a change that cannot land as the sandbox spells it is
//! a conflict, and stays listed until it can.

mod common;

use std::fs;

use common::{
    sandbox,
    spelling::{apply_all, changes, contents, folds_case, pairs, paths, reads_as_one, run},
    tempdir,
};

/// Where the disk reads both names as one, the new name read the same as the
/// disk and was never listed: only the old name's delete was, and applying it
/// deleted the file. On a disk that tells them apart it is a plain rename.
#[tokio::test]
async fn a_rename_that_changes_only_the_case_keeps_the_file() {
    let (_t, dir) = tempdir();
    fs::write(dir.join("readme.md"), "kept\n").unwrap();
    fs::write(dir.join("guide.md"), "guide\n").unwrap();
    let ws = sandbox(&dir).await;
    run(
        &ws,
        &dir,
        "mv readme.md README.md && cp guide.md GUIDE.md && rm guide.md",
    )
    .await;
    apply_all(&ws).await;
    let want = [("GUIDE.md", "guide\n"), ("README.md", "kept\n")];
    assert_eq!(contents(&dir), pairs(&want));
}

/// Where the disk reads both names as one, the edited new name was written
/// first (upper case sorts first), then the old name's delete took it away.
#[tokio::test]
async fn a_case_rename_with_an_edit_keeps_the_edit() {
    let (_t, dir) = tempdir();
    fs::write(dir.join("notes.md"), "notes\n").unwrap();
    let ws = sandbox(&dir).await;
    run(&ws, &dir, "mv notes.md NOTES.md && echo more >> NOTES.md").await;
    apply_all(&ws).await;
    assert_eq!(contents(&dir), pairs(&[("NOTES.md", "notes\nmore\n")]));
}

/// Where the disk reads `Lib` as `lib`, the sandbox's `mv` found the real
/// `lib` under `Lib` and moved `lib` into it: applying wrote `Lib/lib/...`,
/// a tree no shell makes. Real bash renames it, on either disk.
#[tokio::test]
async fn a_directory_renamed_by_case_keeps_its_files() {
    let (_t, dir) = tempdir();
    fs::create_dir(dir.join("lib")).unwrap();
    fs::write(dir.join("lib/a.rs"), "a\n").unwrap();
    fs::write(dir.join("lib/b.rs"), "b\n").unwrap();
    let one = reads_as_one(&dir, "lib", "Lib");
    let ws = sandbox(&dir).await;
    run(&ws, &dir, "mv lib Lib").await;
    apply_all(&ws).await;
    let want = [("Lib/a.rs", "a\n"), ("Lib/b.rs", "b\n")];
    assert_eq!(
        contents(&dir),
        pairs(&want),
        "the disk reads one name: {one}"
    );
}

/// `caf\u{e9}` (one code point) and `cafe\u{301}` (e and a combining accent)
/// are one name on a disk that ignores Unicode normalization: the rename
/// deleted the file as the case-only one did.
#[tokio::test]
async fn a_rename_between_unicode_normalizations_keeps_the_file() {
    let (_t, dir) = tempdir();
    fs::write(dir.join("caf\u{e9}.md"), "menu\n").unwrap();
    let ws = sandbox(&dir).await;
    run(&ws, &dir, "mv caf\u{e9}.md cafe\u{301}.md").await;
    apply_all(&ws).await;
    assert_eq!(contents(&dir), pairs(&[("cafe\u{301}.md", "menu\n")]));
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
    let skipped = paths(&report.skipped);
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
    assert_eq!(skipped, want_skipped, "one name: {one}: {report:?}");
    assert!(report.failed.is_empty(), "{report:?}");
    assert_eq!(contents(&dir), pairs(want_disk), "one name: {one}");
}

/// The sandbox writes `NOTES.md`, a file of its own (it finds `notes.md`
/// only by that spelling), and removes `notes.md`. A human edit made before
/// that `rm` is one the sandbox saw and deleted: both apply, on either disk,
/// as the sandbox shows. One made after the `rm` is a conflict and stays one:
/// the delete is skipped, and where the disk reads both names as one, so is
/// `NOTES.md`, which would land on the human's file.
#[tokio::test]
async fn a_conflict_stays_a_conflict_after_a_delete_under_another_spelling() {
    const HUMAN: &str = "the human was here\n";
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
        let (applied, skipped) = (paths(&report.applied), paths(&report.skipped));
        let case = format!("edit first: {edit_first}, one name: {one}: {report:?}");
        assert!(report.failed.is_empty(), "{case}");
        let disk: &[(&str, &str)] = if edit_first {
            assert_eq!(applied, ["/NOTES.md", "/notes.md"], "{case}");
            &[("NOTES.md", "draft\n")]
        } else if one {
            assert!(applied.is_empty(), "{case}");
            assert_eq!(skipped, ["/NOTES.md", "/notes.md"], "{case}");
            &[("notes.md", HUMAN)]
        } else {
            assert_eq!(applied, ["/NOTES.md"], "{case}");
            assert_eq!(skipped, ["/notes.md"], "{case}");
            &[("NOTES.md", "draft\n"), ("notes.md", HUMAN)]
        };
        assert_eq!(contents(&dir), pairs(disk), "{case}");
        let left = changes(&ws).list().await;
        assert!(left.iter().all(|c| c.conflict), "{case}: left {left:?}");
    }
}

/// The sandbox writes two spellings, neither on the disk before. Where the
/// disk reads them as one file, the first written (upper case sorts first)
/// made the second's real path move: the second was written over it all
/// the same, both were reported applied, and the sandbox's `README.md` was
/// lost, listed nowhere. It is now skipped as a conflict and stays listed.
/// Where the disk tells them apart, both are applied.
#[tokio::test]
async fn two_spellings_the_sandbox_wrote_never_overwrite_each_other() {
    let (_t, dir) = tempdir();
    let one = folds_case(&dir);
    let ws = sandbox(&dir).await;
    let command = "echo upper > README.md && echo lower > readme.md";
    run(&ws, &dir, command).await;
    let report = changes(&ws).apply().await;
    assert!(report.failed.is_empty(), "{report:?}");
    let left = changes(&ws).list().await;
    assert!(left.iter().chain(&report.skipped).all(|c| c.conflict));
    if one {
        assert_eq!(paths(&report.applied), ["/README.md"], "{report:?}");
        assert_eq!(paths(&report.skipped), ["/readme.md"], "{report:?}");
        assert_eq!(contents(&dir), pairs(&[("README.md", "upper\n")]));
        assert_eq!(paths(&left), ["/readme.md"], "{report:?}");
    } else {
        let both = ["/README.md", "/readme.md"];
        assert_eq!(paths(&report.applied), both, "{report:?}");
        assert!(report.skipped.is_empty(), "{report:?}");
        let want = [("README.md", "upper\n"), ("readme.md", "lower\n")];
        assert_eq!(contents(&dir), pairs(&want));
        assert!(left.is_empty(), "{left:?}");
    }
}

/// The same with two spellings of a directory. Where the disk reads them as
/// one, it holds one directory: the spelling written first is applied whole,
/// and the other, with everything in it, would land in that one, so it is
/// skipped as a conflict. Its files went into the first directory and were
/// reported applied, so nothing listed them any more: removing the first
/// spelling in the sandbox later deleted them on apply, while the sandbox
/// still showed them under the second. Where the disk tells the spellings
/// apart, both directories are applied whole.
#[tokio::test]
async fn two_spellings_of_a_directory_apply_one_and_the_other_conflicts() {
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
    assert!(report.failed.is_empty(), "{report:?}");
    let left = changes(&ws).list().await;
    assert!(left.iter().chain(&report.skipped).all(|c| c.conflict));
    if one {
        let applied = ["/Lib", "/Lib/a.rs", "/Lib/same.rs"];
        assert_eq!(paths(&report.applied), applied, "{report:?}");
        let skipped = ["/lib", "/lib/b.rs", "/lib/same.rs"];
        assert_eq!(paths(&report.skipped), skipped, "{report:?}");
        let want = [("Lib/a.rs", "a\n"), ("Lib/same.rs", "upper\n")];
        assert_eq!(contents(&dir), pairs(&want));
        assert_eq!(paths(&left), skipped, "{report:?}");
    } else {
        assert_eq!(report.applied.len(), 6, "{report:?}");
        assert!(report.skipped.is_empty(), "{report:?}");
        let want = [
            ("Lib/a.rs", "a\n"),
            ("Lib/same.rs", "upper\n"),
            ("lib/b.rs", "b\n"),
            ("lib/same.rs", "lower\n"),
        ];
        assert_eq!(contents(&dir), pairs(&want));
        assert!(left.is_empty(), "{left:?}");
    }
    // The model tidies the first spelling away: the disk ends as the
    // sandbox shows it, on either disk.
    run(&ws, &dir, "rm -r Lib").await;
    apply_all(&ws).await;
    let want = [("lib/b.rs", "b\n"), ("lib/same.rs", "lower\n")];
    assert_eq!(contents(&dir), pairs(&want), "one name: {one}");
}

/// A name the sandbox writes under another spelling of a file or a
/// directory the disk holds, the content alike or not. The write landed on
/// the disk's spelling, was reported applied and left the change list, and
/// the sandbox kept showing the two apart. Removing the old spelling in a
/// later round deleted both on apply: the disk ended without the file the
/// sandbox still showed. (The last case reads as one name on any APFS.)
#[tokio::test]
async fn a_write_under_another_spelling_of_what_the_disk_holds_is_never_forgotten() {
    /// The disk's one file (`x\n`), the sandbox's two rounds, and the one
    /// file the disk holds after both.
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
        let one = folds_case(&dir);
        let ws = sandbox(&dir).await;
        run(&ws, &dir, first).await;
        let report = changes(&ws).apply().await;
        // What is not applied is a conflict, and stays listed.
        assert!(report.failed.is_empty(), "{first}: {report:?}");
        let left = changes(&ws).list().await;
        assert_eq!(paths(&left), paths(&report.skipped), "{first}");
        assert!(left.iter().all(|c| c.conflict), "{first}: {left:?}");
        run(&ws, &dir, second).await;
        apply_all(&ws).await;
        let case = format!("{first}, then {second}; one name: {one}; first apply {report:?}");
        assert_eq!(contents(&dir), pairs(&[want]), "{case}");
    }
}

/// A rename by case whose delete fails partway (a read-only directory inside
/// the old name). The new name's writes landed in the old directory, where
/// the disk reads both names as one, and were reported applied; only the
/// delete stayed listed, and applying again once the human made the
/// directory writable deleted every file. (As root nothing is read-only, and
/// the rename applies at once.)
#[tokio::test]
async fn a_rename_by_case_whose_delete_fails_writes_nothing_into_the_old_name() {
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
    // The delete took what it could: `lib/ro` is still there unless it went.
    let _ = mode(0o755);
    let second = changes(&ws).apply().await;
    let want = [("Lib/a.rs", "a\n"), ("Lib/ro/x.txt", "x\n")];
    assert_eq!(
        contents(&dir),
        pairs(&want),
        "first {first:?}, second {second:?}"
    );
    let left = changes(&ws).list().await;
    assert!(left.is_empty(), "left {left:?}");
}

/// A tree made beside a file the disk spells another way (`NOTES/x` beside
/// the file `notes`). The disk answers "not a directory" for `NOTES/x`, and
/// that ended the search for how the disk spells the way, so the file below
/// it was a plain add, and applying it failed with "Not a directory". It is a
/// conflict, as the directories above it are, until the file goes.
#[tokio::test]
async fn a_write_below_a_directory_beside_a_file_spelled_another_way_is_a_conflict() {
    let (_t, dir) = tempdir();
    fs::write(dir.join("notes"), "n\n").unwrap();
    let one = reads_as_one(&dir, "notes", "NOTES");
    let ws = sandbox(&dir).await;
    run(&ws, &dir, "mkdir -p NOTES/x && echo y > NOTES/x/y").await;
    let report = changes(&ws).apply().await;
    assert!(report.failed.is_empty(), "{report:?}");
    let left = changes(&ws).list().await;
    assert_eq!(paths(&left), paths(&report.skipped), "{report:?}");
    assert!(left.iter().all(|c| c.conflict), "{left:?}");
    if one {
        let want = ["/NOTES/x", "/NOTES/x/y"];
        assert_eq!(paths(&report.skipped), want, "{report:?}");
        assert_eq!(contents(&dir), pairs(&[("notes", "n\n")]));
    }
    run(&ws, &dir, "rm notes").await;
    apply_all(&ws).await;
    assert_eq!(
        contents(&dir),
        pairs(&[("NOTES/x/y", "y\n")]),
        "one name: {one}"
    );
}
