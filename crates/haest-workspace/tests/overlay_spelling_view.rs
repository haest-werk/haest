//! What the sandbox shows and lists over a disk that reads some spellings of
//! a name as one (macOS's default: case and Unicode normalization;
//! case-sensitive APFS: normalization only). The sandbox finds a project path
//! only as the disk spells it, so it shows what applying will write. Every
//! test holds on a disk that tells spellings apart too.

mod common;

use std::fs;

use common::{
    opts, sandbox,
    spelling::{apply_all, changes, contents, pairs, paths, reads_as_one, run},
    tempdir,
};
use haest_core::workspace::{ApplyFailureCode, ChangeKind, Workspace};

/// Where the disk reads `Lib` as `lib`, the sandbox showed the files of a
/// `lib` it had renamed or deleted under `Lib/`, read through from the disk.
/// `mv lib Lib` moved `lib` INTO that `Lib` (`Lib/lib/a.rs` on apply), and
/// the model saw each file twice; removing the nested copy left only `lib`'s
/// delete, which took every file on apply. The two-step rename applied
/// `Lib/tmp/a.rs`, and `mkdir Lib` after `rm -r lib` failed: "File exists".
#[tokio::test]
async fn the_sandbox_shows_no_file_of_a_directory_renamed_by_case() {
    for (command, body) in [
        ("mv lib Lib", "a\n"),
        ("mv lib Lib && rm -rf Lib/lib", "a\n"),
        ("mv lib tmp && mv tmp Lib", "a\n"),
        ("rm -r lib && mkdir Lib && echo new > Lib/a.rs", "new\n"),
    ] {
        let (_t, dir) = tempdir();
        fs::create_dir(dir.join("lib")).unwrap();
        fs::write(dir.join("lib/a.rs"), "a\n").unwrap();
        let one = reads_as_one(&dir, "lib", "Lib");
        let ws = sandbox(&dir).await;
        run(&ws, &dir, command).await;
        let r = ws.shell("find . -type f | sort", opts(&dir)).await;
        assert_eq!(r.stdout, "./Lib/a.rs\n", "{command}, one name: {one}");
        apply_all(&ws).await;
        let want = pairs(&[("Lib/a.rs", body)]);
        assert_eq!(contents(&dir), want, "{command}, one name: {one}");
    }
}

/// The same between Unicode normalizations, which case-sensitive APFS reads
/// as one name too: `mv caf\u{e9} cafe\u{301}` applied
/// `cafe\u{301}/caf\u{e9}/menu.md`.
#[tokio::test]
async fn a_directory_renamed_between_unicode_normalizations_keeps_its_files() {
    let (_t, dir) = tempdir();
    fs::create_dir(dir.join("caf\u{e9}")).unwrap();
    fs::write(dir.join("caf\u{e9}/menu.md"), "m\n").unwrap();
    let ws = sandbox(&dir).await;
    run(&ws, &dir, "mv caf\u{e9} cafe\u{301}").await;
    let r = ws.shell("find . -type f", opts(&dir)).await;
    assert_eq!(r.stdout, "./cafe\u{301}/menu.md\n");
    apply_all(&ws).await;
    assert_eq!(contents(&dir), pairs(&[("cafe\u{301}/menu.md", "m\n")]));
}

/// The human reviews a rename as one: the old name deleted, the new one
/// added. Where the disk reads both names as one, the new name read as the
/// old one's file and was not listed at all: `mv lib Lib` listed only
/// "delete /lib".
#[tokio::test]
async fn a_rename_by_case_lists_the_old_name_deleted_and_the_new_one_added() {
    use ChangeKind::{Added, Deleted};
    let file: &[(&str, ChangeKind)] = &[("/README.md", Added), ("/readme.md", Deleted)];
    let dir: &[(&str, ChangeKind)] = &[("/Lib", Added), ("/Lib/a.rs", Added), ("/lib", Deleted)];
    let cases = [
        ("readme.md", "mv readme.md README.md", file),
        ("lib/a.rs", "mv lib Lib", dir),
    ];
    for (file, command, want) in cases {
        let (_t, dir) = tempdir();
        fs::create_dir_all(dir.join(file).parent().unwrap()).unwrap();
        fs::write(dir.join(file), "x\n").unwrap();
        let ws = sandbox(&dir).await;
        run(&ws, &dir, command).await;
        let listed = changes(&ws).list().await;
        let got: Vec<_> = listed.iter().map(|c| (c.path.as_str(), c.kind)).collect();
        assert_eq!(got, want, "{command}: {listed:?}");
        assert!(listed.iter().all(|c| !c.conflict), "{command}: {listed:?}");
    }
}

/// The sandbox finds a project path only as the disk spells it, where real
/// bash would find it under another spelling too: `cat README.md` does not
/// find `readme.md`, and `echo new > README.md` makes a second file. Where
/// the disk reads both names as one, applying that file would overwrite
/// `readme.md`, which the sandbox shows unchanged: it is listed as a
/// conflict, and skipped.
#[tokio::test]
async fn the_sandbox_finds_a_path_only_as_the_disk_spells_it() {
    let (_t, dir) = tempdir();
    fs::write(dir.join("readme.md"), "old\n").unwrap();
    fs::write(dir.join("caf\u{e9}.md"), "menu\n").unwrap();
    let one = reads_as_one(&dir, "readme.md", "README.md");
    let ws = sandbox(&dir).await;
    for name in ["README.md", "cafe\u{301}.md"] {
        let r = ws.shell(&format!("cat {name}"), opts(&dir)).await;
        assert_eq!(r.exit_code, 1, "cat {name}, one name: {one}: {r:?}");
    }
    run(&ws, &dir, "echo new > README.md").await;
    let r = ws.shell("ls", opts(&dir)).await;
    assert_eq!(r.stdout, "README.md\ncaf\u{e9}.md\nreadme.md\n");
    let listed = changes(&ws).list().await;
    let got: Vec<_> = listed
        .iter()
        .map(|c| (c.path.as_str(), c.kind, c.conflict))
        .collect();
    assert_eq!(
        got,
        [("/README.md", ChangeKind::Added, one)],
        "one name: {one}"
    );
    let report = changes(&ws).apply().await;
    assert_eq!(report.skipped.len(), usize::from(one), "{report:?}");
    let mut want = vec![("caf\u{e9}.md", "menu\n"), ("readme.md", "old\n")];
    if !one {
        want.insert(0, ("README.md", "new\n"));
    }
    assert_eq!(contents(&dir), pairs(&want), "one name: {one}");
}

/// A symlink that does not resolve (dangling, or one of a loop) answers no
/// `realpath`, so it read as asked under any spelling: after `mv lib Lib`
/// the disk's `lib/d` stood in for `/Lib/d`, which was never listed, and
/// apply's delete of `/lib` took it while the report said every change was
/// applied. It is listed now, and refused like any link the sandbox made.
/// (The last case reads as one name on any APFS.)
#[tokio::test]
async fn a_link_that_does_not_resolve_in_a_renamed_directory_is_listed() {
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
        let ws = sandbox(&dir).await;
        run(&ws, &dir, command).await;
        let links = ["d", "l1", "l2"].map(|l| format!("/{new}/{l}"));
        let mut want = vec![format!("/{new}"), format!("/{new}/a.rs"), format!("/{old}")];
        want.extend(links.clone());
        want.sort();
        assert_eq!(paths(&changes(&ws).list().await), want, "{command}");
        let report = changes(&ws).apply().await;
        let mut failed: Vec<_> = report
            .failed
            .iter()
            .map(|f| (f.change.path.clone(), f.code))
            .collect();
        failed.sort_by(|a, b| a.0.cmp(&b.0));
        let refused = links.map(|l| (l, ApplyFailureCode::Symlink));
        assert_eq!(failed, refused, "{command}: {report:?}");
        assert!(report.skipped.is_empty(), "{command}: {report:?}");
    }
}
