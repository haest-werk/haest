//! Applying on a disk that reads some spellings of a name as one (macOS's
//! default: case and Unicode normalization). The sandbox's overlay tells
//! `readme.md` and `README.md` apart; under it the disk holds one file for
//! both. Every test holds on a disk that tells them apart too, and says what
//! each kind of disk ends up holding.

mod common;

use std::{fs, path::Path};

use common::{opts, sandbox, tempdir};
use haest_core::workspace::{Change, ChangeSetRef, Workspace};
use haest_workspace::SandboxWorkspace;

async fn run(ws: &impl Workspace, dir: &Path, command: &str) {
    let r = ws.shell(command, opts(dir)).await;
    assert_eq!(r.exit_code, 0, "{command}: {r:?}");
}

fn changes(ws: &SandboxWorkspace) -> ChangeSetRef {
    ws.changes().expect("the sandbox records its changes")
}

/// Whether this disk reads `other` as the name `name` already in `dir`.
fn reads_as_one(dir: &Path, name: &str, other: &str) -> bool {
    assert!(dir.join(name).exists(), "{name} is in {}", dir.display());
    dir.join(other).exists()
}

/// Whether the disk under `dir` reads names without case, asked with a file
/// that is gone again once it answers.
fn folds_case(dir: &Path) -> bool {
    fs::write(dir.join("probe"), "").unwrap();
    let one = reads_as_one(dir, "probe", "PROBE");
    fs::remove_file(dir.join("probe")).unwrap();
    one
}

/// Every file under `dir`, by its path inside it as the disk lists it, with
/// its content.
fn contents(dir: &Path) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut pending = vec![dir.to_path_buf()];
    while let Some(d) = pending.pop() {
        for e in fs::read_dir(&d).unwrap() {
            let p = e.unwrap().path();
            if p.is_dir() {
                pending.push(p);
            } else {
                let rel = p.strip_prefix(dir).unwrap().to_string_lossy().into_owned();
                out.push((rel, fs::read_to_string(&p).unwrap()));
            }
        }
    }
    out.sort();
    out
}

/// The changes' paths, sorted: apply reports the deletes first.
fn paths(changes: &[Change]) -> Vec<String> {
    let mut all: Vec<_> = changes.iter().map(|c| c.path.clone()).collect();
    all.sort();
    all
}

fn pairs(want: &[(&str, &str)]) -> Vec<(String, String)> {
    want.iter()
        .map(|(p, b)| ((*p).to_owned(), (*b).to_owned()))
        .collect()
}

/// Apply everything the sandbox changed; nothing may be skipped or fail, and
/// nothing is left listed.
async fn apply_all(ws: &SandboxWorkspace) {
    let report = changes(ws).apply().await;
    assert!(
        report.failed.is_empty() && report.skipped.is_empty(),
        "{report:?}"
    );
    let left = changes(ws).list().await;
    assert!(left.is_empty(), "left {left:?} after {report:?}");
}

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

/// Where the disk reads `Lib` as `lib`, the sandbox's `mv` finds `Lib`
/// there and moves `lib` into it; applying wrote `lib/lib/...`, then the
/// delete of `lib` took the whole directory. On a disk that tells them apart
/// it is a plain rename.
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
    let want = if one {
        [("Lib/lib/a.rs", "a\n"), ("Lib/lib/b.rs", "b\n")]
    } else {
        [("Lib/a.rs", "a\n"), ("Lib/b.rs", "b\n")]
    };
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

/// A conflict when the apply starts is skipped, whatever its deletes do to
/// the disk. The sandbox writes `NOTES.md`, the human then edits the file,
/// and the sandbox removes `notes.md`. Where the disk reads both names as
/// one, `NOTES.md` was written over a file the human changed since: it stays
/// a conflict even once the delete of `notes.md` (which saw the human's
/// version) has removed the file. Where the disk tells them apart, both
/// apply.
#[tokio::test]
async fn a_conflict_stays_a_conflict_after_a_delete_under_another_spelling() {
    let (_t, dir) = tempdir();
    fs::write(dir.join("notes.md"), "v1\n").unwrap();
    let one = reads_as_one(&dir, "notes.md", "NOTES.md");
    let ws = sandbox(&dir).await;
    run(&ws, &dir, "echo draft > NOTES.md").await;
    fs::write(dir.join("notes.md"), "the human was here\n").unwrap();
    run(&ws, &dir, "rm notes.md").await;
    let report = changes(&ws).apply().await;
    let (applied, skipped) = (paths(&report.applied), paths(&report.skipped));
    if one {
        assert_eq!(applied, ["/notes.md"], "{report:?}");
        assert_eq!(skipped, ["/NOTES.md"], "{report:?}");
        assert!(changes(&ws).list().await.iter().all(|c| c.conflict));
    } else {
        assert_eq!(applied, ["/NOTES.md", "/notes.md"], "{report:?}");
        assert!(skipped.is_empty(), "{report:?}");
        assert_eq!(contents(&dir), pairs(&[("NOTES.md", "draft\n")]));
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
/// one, their files go into the one directory, which was written first and
/// does not make them look moved; only the file both spell alike is skipped
/// as a conflict. Where the disk tells them apart, both directories are
/// applied whole.
#[tokio::test]
async fn two_spellings_of_a_directory_share_it_and_only_a_file_in_both_conflicts() {
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
        let applied = ["/Lib", "/Lib/a.rs", "/Lib/same.rs", "/lib", "/lib/b.rs"];
        assert_eq!(paths(&report.applied), applied, "{report:?}");
        assert_eq!(paths(&report.skipped), ["/lib/same.rs"], "{report:?}");
        let want = [
            ("Lib/a.rs", "a\n"),
            ("Lib/b.rs", "b\n"),
            ("Lib/same.rs", "upper\n"),
        ];
        assert_eq!(contents(&dir), pairs(&want));
        assert_eq!(paths(&left), ["/lib/same.rs"], "{report:?}");
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
}
