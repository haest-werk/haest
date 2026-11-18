//! A directory the sandbox removed (or moved away) and made again: what the
//! sandbox shows under it is what applying leaves on the disk.

mod common;

use std::fs;

use common::{
    sandbox,
    spelling::{apply_all, contents, pairs, run},
    tempdir,
};
use haest_core::{fs::ensure_dir, workspace::Workspace};

/// `rm -rf dist && mkdir -p dist/sub`: the overlay's recursive mkdir cleared
/// the whiteout of `dist/sub` alone, so `dist` stayed removed and hid all
/// made under it. Every command said it succeeded, `cat` found nothing, and
/// applying deleted `dist` and wrote none of it. One level down, `rm -rf`
/// left a whiteout at `dist/sub` too.
#[tokio::test]
async fn a_tree_made_with_mkdir_p_under_a_removed_directory_is_kept() {
    for (command, file) in [
        (
            "rm -rf dist && mkdir -p dist/sub && echo c > dist/sub/c.js",
            "dist/sub/c.js",
        ),
        (
            "rm -rf dist && mkdir dist && mkdir -p dist/sub/x && echo c > dist/sub/x/c.js",
            "dist/sub/x/c.js",
        ),
    ] {
        let (_t, dir) = tempdir();
        fs::create_dir_all(dir.join("dist/sub")).unwrap();
        fs::write(dir.join("dist/a.js"), "a\n").unwrap();
        fs::write(dir.join("dist/sub/b.js"), "b\n").unwrap();
        let ws = sandbox(&dir).await;
        run(&ws, &dir, command).await;
        run(&ws, &dir, &format!("cat {file}")).await;
        apply_all(&ws).await;
        assert_eq!(contents(&dir), pairs(&[(file, "c\n")]), "{command}");
    }
}

/// The Write tool makes a file's parent with `ensure_dir` (a recursive
/// mkdir): under a directory the shell removed, the file it reported
/// created read back as missing, and was never applied.
#[tokio::test]
async fn a_file_written_under_a_removed_directory_reads_back() {
    let (_t, dir) = tempdir();
    fs::create_dir_all(dir.join("src/old")).unwrap();
    fs::write(dir.join("src/old/a.rs"), "a\n").unwrap();
    let ws = sandbox(&dir).await;
    run(&ws, &dir, "rm -rf src/old").await;
    let files = ws.fs();
    let file = dir.join("src/old/new/b.rs");
    ensure_dir(files.as_ref(), file.parent().unwrap())
        .await
        .unwrap();
    files.write(&file, b"b\n").await.unwrap();
    assert_eq!(files.read(&file).await.unwrap(), b"b\n");
    apply_all(&ws).await;
    assert_eq!(contents(&dir), pairs(&[("src/old/new/b.rs", "b\n")]));
}

/// `rm -r lib && mkdir lib`: the files the sandbox removed with `lib` were
/// never listed, so applying left them on the disk while the sandbox showed
/// `lib` without them, and the report said everything was applied.
#[tokio::test]
async fn files_removed_with_a_directory_made_again_are_deleted_on_apply() {
    for command in [
        "rm -r lib && mkdir lib && echo c > lib/c.rs",
        "mv lib old && mkdir lib && echo c > lib/c.rs && rm -r old",
    ] {
        let (_t, dir) = tempdir();
        fs::create_dir_all(dir.join("lib/sub")).unwrap();
        fs::write(dir.join("lib/a.rs"), "a\n").unwrap();
        fs::write(dir.join("lib/sub/b.rs"), "b\n").unwrap();
        let ws = sandbox(&dir).await;
        run(&ws, &dir, command).await;
        apply_all(&ws).await;
        assert_eq!(contents(&dir), pairs(&[("lib/c.rs", "c\n")]), "{command}");
    }
}

/// `rm -rf dist && mv build dist` (a fresh build swapped in): the moved tree
/// records only `dist`, which was a directory before and after, so neither
/// the new files, the changed one nor the deleted one was listed or applied.
#[tokio::test]
async fn a_tree_moved_onto_a_removed_directory_is_applied() {
    let (_t, dir) = tempdir();
    fs::create_dir_all(dir.join("dist/sub")).unwrap();
    fs::write(dir.join("dist/a.js"), "a\n").unwrap();
    fs::write(dir.join("dist/sub/b.js"), "b\n").unwrap();
    let ws = sandbox(&dir).await;
    run(
        &ws,
        &dir,
        "mkdir -p build/sub && echo x > build/sub/x.js && echo y > build/a.js && rm -rf dist && mv build dist",
    )
    .await;
    apply_all(&ws).await;
    let want = [("dist/a.js", "y\n"), ("dist/sub/x.js", "x\n")];
    assert_eq!(contents(&dir), pairs(&want));
}

/// A tree moved onto an empty directory the disk holds (a `rename` through
/// the sandbox's files; `mv` moves into it): only that directory was
/// recorded, a directory before and after, so the tree was never applied.
#[tokio::test]
async fn a_tree_renamed_onto_an_empty_directory_is_applied() {
    let (_t, dir) = tempdir();
    fs::create_dir(dir.join("dist")).unwrap();
    let ws = sandbox(&dir).await;
    run(&ws, &dir, "mkdir -p build/sub && echo x > build/sub/x.js").await;
    let files = ws.fs();
    files
        .rename(&dir.join("build"), &dir.join("dist"))
        .await
        .unwrap();
    apply_all(&ws).await;
    assert_eq!(contents(&dir), pairs(&[("dist/sub/x.js", "x\n")]));
}

/// Once applied, a directory made again is done with: a file the human
/// makes in it later, under a name the sandbox removed, is theirs, and no
/// apply deletes it.
#[tokio::test]
async fn a_file_the_human_makes_after_the_apply_is_not_deleted_by_the_next() {
    let (_t, dir) = tempdir();
    fs::create_dir(dir.join("lib")).unwrap();
    fs::write(dir.join("lib/a.rs"), "a\n").unwrap();
    let ws = sandbox(&dir).await;
    run(&ws, &dir, "rm -r lib && mkdir lib && echo c > lib/c.rs").await;
    apply_all(&ws).await;
    assert_eq!(contents(&dir), pairs(&[("lib/c.rs", "c\n")]));
    fs::write(dir.join("lib/a.rs"), "human\n").unwrap();
    apply_all(&ws).await;
    let want = [("lib/a.rs", "human\n"), ("lib/c.rs", "c\n")];
    assert_eq!(contents(&dir), pairs(&want));
}
