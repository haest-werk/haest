//! Where bash makes a path in the project, or refuses to, the sandbox does
//! the same, and applying puts on the disk what the sandbox shows.

mod common;

use std::fs;

use common::{
    opts,
    project::{apply_all, changes, contents, nothing_lost, pairs, paths, reads_as_one, run},
    sandbox, tempdir,
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
    assert_eq!(contents(&dir), pairs(&[]));
}

/// `mkdir -p` through a file fails in bash ("Not a directory"). In the
/// sandbox it made a directory over the file, which then was gone there,
/// and listed one that applying could only fail.
#[tokio::test]
async fn mkdir_through_a_file_fails_as_in_bash() {
    let (_t, dir) = tempdir();
    fs::write(dir.join("notes"), "n\n").unwrap();
    let ws = sandbox(&dir).await;
    run(&ws, &dir, "echo m > mine").await;
    for script in [
        "mkdir -p notes/x/y",
        "mkdir -p notes/x",
        "mkdir notes/x",
        "mkdir -p mine/x/y",
    ] {
        let r = ws.shell(script, opts(&dir)).await;
        assert_ne!(r.exit_code, 0, "{script}: {r:?}");
    }
    let r = ws.shell("cat notes mine", opts(&dir)).await;
    assert_eq!(r.stdout, "n\nm\n", "{r:?}");
    apply_all(&ws).await;
    assert_eq!(contents(&dir), pairs(&[("mine", "m\n"), ("notes", "n\n")]));
}

/// A move onto the path the file is at leaves it, in bash. bashkit moves a
/// file by writing the new name and removing the old one, so in the sandbox
/// that removed the file, and applying deleted it from the disk. A loop
/// that renames files to a name some of them already have (lowercasing,
/// `${f// /_}`) took exactly those. Where the disk reads `Todo.md` and
/// `todo.md` as one, that rename by case is held as a conflict, and the
/// disk keeps `Todo.md`.
#[tokio::test]
async fn a_move_onto_itself_keeps_the_file() {
    use std::os::unix::fs::PermissionsExt;
    let (_t, dir) = tempdir();
    fs::create_dir(dir.join("d")).unwrap();
    let want = [
        ("Todo.md", "t\n"),
        ("a", "a\n"),
        ("d/b", "b\n"),
        ("d/c", "c\n"),
        ("my notes.txt", "m\n"),
        ("notes.md", "n\n"),
        ("secret.key", "k\n"),
    ];
    for (name, body) in want {
        fs::write(dir.join(name), body).unwrap();
    }
    let one = reads_as_one(&dir, "Todo.md", "todo.md");
    let ws = sandbox(&dir).await;
    run(
        &ws,
        &dir,
        "mv a ./a && mv d/b d && mv d/c d/../d/c \
         && for f in *.md; do mv \"$f\" \"$(echo \"$f\" | tr A-Z a-z)\"; done \
         && for f in *.txt; do mv \"$f\" \"${f// /_}\"; done \
         && chmod 600 secret.key && mv secret.key ./secret.key",
    )
    .await;
    let r = ws
        .shell(
            "cat a d/b d/c notes.md todo.md my_notes.txt secret.key",
            opts(&dir),
        )
        .await;
    assert_eq!(r.stdout, "a\nb\nc\nn\nt\nm\nk\n", "{r:?}");
    let todo = if one {
        let report = changes(&ws).apply().await;
        nothing_lost(&ws, &dir, &report).await;
        assert_eq!(paths(&report.skipped), ["/Todo.md"], "{report:?}");
        "Todo.md"
    } else {
        apply_all(&ws).await;
        "todo.md"
    };
    let want = [
        ("a", "a\n"),
        ("d/b", "b\n"),
        ("d/c", "c\n"),
        ("my_notes.txt", "m\n"),
        ("notes.md", "n\n"),
        ("secret.key", "k\n"),
        (todo, "t\n"),
    ];
    let mut want = pairs(&want);
    want.sort();
    assert_eq!(contents(&dir), want);
    let key = fs::metadata(dir.join("secret.key")).unwrap().permissions();
    assert_eq!(key.mode() & 0o7777, 0o600);
}
