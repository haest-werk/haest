//! Where bash makes a path in the project, or refuses to, the sandbox does
//! the same, and applying puts on the disk what the sandbox shows.

mod common;

use std::fs;

use common::{
    opts, sandbox,
    spelling::{apply_all, contents, pairs, run},
    tempdir,
};
use haest_core::workspace::Workspace;

/// `>>` onto a file that is not there makes it, in bash. In the sandbox it
/// failed in a directory only the disk held (every one it had not written
/// in yet) and after `rm`, and `tee -a` failed the whole script.
#[tokio::test]
async fn appending_to_a_file_that_is_not_there_makes_it() {
    let (_t, dir) = tempdir();
    fs::create_dir(dir.join("lib")).unwrap();
    fs::write(dir.join("lib/a.rs"), "a\n").unwrap();
    let ws = sandbox(&dir).await;
    run(&ws, &dir, "echo x >> lib/new.rs").await;
    run(&ws, &dir, "echo z | tee -a lib/t.rs > /dev/null").await;
    run(&ws, &dir, "rm lib/a.rs && echo y >> lib/a.rs").await;
    run(&ws, &dir, "cat >> lib/doc.md <<EOF\nhi\nEOF").await;
    apply_all(&ws).await;
    let want = [
        ("lib/a.rs", "y\n"),
        ("lib/doc.md", "hi\n"),
        ("lib/new.rs", "x\n"),
        ("lib/t.rs", "z\n"),
    ];
    assert_eq!(contents(&dir), pairs(&want));
}

/// Under a directory the sandbox removed, `>>` still fails, as in bash.
#[tokio::test]
async fn appending_under_a_removed_directory_fails_as_in_bash() {
    let (_t, dir) = tempdir();
    fs::create_dir(dir.join("lib")).unwrap();
    fs::write(dir.join("lib/a.rs"), "a\n").unwrap();
    let ws = sandbox(&dir).await;
    run(&ws, &dir, "rm -r lib").await;
    let r = ws.shell("echo x >> lib/a.rs", opts(&dir)).await;
    assert_ne!(r.exit_code, 0, "{r:?}");
    apply_all(&ws).await;
    assert!(contents(&dir).is_empty());
}
