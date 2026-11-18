//! The sandbox goes on after an apply, as haest's command does mid-session:
//! what was applied reads through to the disk again, so what the human does
//! there afterwards is what the sandbox sees and starts from, never written
//! over without a conflict.

mod common;

use std::fs;

use common::{
    opts, sandbox,
    spelling::{apply_all, contents, pairs, run},
    tempdir,
};
use haest_core::workspace::Workspace;

/// The human edits a file after its change was applied. The sandbox still
/// read its own copy from before, and its next write went over the human's
/// edit with no conflict.
#[tokio::test]
async fn an_edit_the_human_makes_after_an_apply_is_never_written_over() {
    let (_t, dir) = tempdir();
    fs::write(dir.join("a.md"), "v1\n").unwrap();
    let ws = sandbox(&dir).await;
    run(&ws, &dir, "echo v2 > a.md").await;
    apply_all(&ws).await;
    fs::write(dir.join("a.md"), "v2\nhuman\n").unwrap();
    let r = ws.shell("cat a.md", opts(&dir)).await;
    assert_eq!(
        r.stdout, "v2\nhuman\n",
        "the sandbox reads the human's edit"
    );
    run(&ws, &dir, "echo model >> a.md").await;
    apply_all(&ws).await;
    assert_eq!(contents(&dir), pairs(&[("a.md", "v2\nhuman\nmodel\n")]));
}

/// A file the sandbox deleted, applied, then made again by the human: the
/// sandbox could not see it, so `test -f a.md || ...` took it for missing,
/// and its next change deleted it or wrote over it.
#[tokio::test]
async fn a_file_the_human_makes_after_an_applied_delete_is_seen() {
    let (_t, dir) = tempdir();
    fs::write(dir.join("a.md"), "v1\n").unwrap();
    let ws = sandbox(&dir).await;
    run(&ws, &dir, "rm a.md").await;
    apply_all(&ws).await;
    fs::write(dir.join("a.md"), "human\n").unwrap();
    let r = ws.shell("cat a.md", opts(&dir)).await;
    assert_eq!(r.stdout, "human\n", "the sandbox reads the human's file");
    run(&ws, &dir, "echo model >> a.md").await;
    apply_all(&ws).await;
    assert_eq!(contents(&dir), pairs(&[("a.md", "human\nmodel\n")]));
}

/// The same for a directory the sandbox removed whole: the files the human
/// makes in it again, under the names it had, are seen too.
#[tokio::test]
async fn a_directory_the_human_makes_after_an_applied_delete_is_seen() {
    let (_t, dir) = tempdir();
    fs::create_dir_all(dir.join("build/js")).unwrap();
    fs::write(dir.join("build/js/out.js"), "old\n").unwrap();
    let ws = sandbox(&dir).await;
    run(&ws, &dir, "rm -r build").await;
    apply_all(&ws).await;
    fs::create_dir_all(dir.join("build/js")).unwrap();
    fs::write(dir.join("build/js/out.js"), "human\n").unwrap();
    let r = ws.shell("cat build/js/out.js", opts(&dir)).await;
    assert_eq!(r.stdout, "human\n", "{r:?}");
    run(&ws, &dir, "echo model >> build/js/out.js").await;
    apply_all(&ws).await;
    assert_eq!(
        contents(&dir),
        pairs(&[("build/js/out.js", "human\nmodel\n")])
    );
}

/// A file the human deletes after its change was applied is gone in the
/// sandbox too: nothing writes it back.
#[tokio::test]
async fn a_file_the_human_deletes_after_an_apply_is_gone_in_the_sandbox() {
    let (_t, dir) = tempdir();
    fs::write(dir.join("a.md"), "v1\n").unwrap();
    let ws = sandbox(&dir).await;
    run(&ws, &dir, "echo v2 > a.md").await;
    apply_all(&ws).await;
    fs::remove_file(dir.join("a.md")).unwrap();
    let r = ws.shell("test -e a.md", opts(&dir)).await;
    assert_ne!(r.exit_code, 0, "the sandbox sees the human's delete");
}

/// A directory the sandbox made, applied, then removed by the human is gone
/// in the sandbox too.
#[tokio::test]
async fn a_directory_the_human_removes_after_an_apply_is_gone_in_the_sandbox() {
    let (_t, dir) = tempdir();
    let ws = sandbox(&dir).await;
    run(&ws, &dir, "mkdir -p src/sub && echo x > src/sub/a.rs").await;
    apply_all(&ws).await;
    fs::remove_dir_all(dir.join("src")).unwrap();
    let r = ws.shell("test -e src", opts(&dir)).await;
    assert_ne!(r.exit_code, 0, "the sandbox sees the human's delete");
}
