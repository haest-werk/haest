//! What /apply may not do to the real disk, whatever the sandbox wrote:
//! reach through a symlink in the project, out of it or anywhere else.

mod common;

use std::{fs, path::Path};

use common::{opts, sandbox, tempdir};
use haest_core::workspace::{ApplyFailureCode, Workspace};

async fn run(ws: &impl Workspace, dir: &Path, command: &str) {
    let r = ws.shell(command, opts(dir)).await;
    assert_eq!(r.exit_code, 0, "{command}: {r:?}");
}

fn names(dir: &Path) -> Vec<String> {
    let mut all: Vec<String> = fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    all.sort();
    all
}

fn is_link(p: &Path) -> bool {
    fs::symlink_metadata(p).is_ok_and(|m| m.file_type().is_symlink())
}

#[cfg(unix)]
#[tokio::test]
async fn apply_never_writes_or_deletes_through_a_symlink_in_the_project() {
    use std::os::unix::fs::symlink;
    let (_t, dir) = tempdir();
    let (_o, outside) = tempdir();
    fs::write(outside.join("authorized_keys"), "mine\n").unwrap();
    fs::write(outside.join("hosts"), "127.0.0.1 localhost\n").unwrap();
    fs::write(dir.join("inner.txt"), "inner\n").unwrap();
    fs::write(dir.join("ok.txt"), "old\n").unwrap();
    // A directory out of the project, a file out of it, a file inside it.
    symlink(&outside, dir.join("vendor")).unwrap();
    symlink(outside.join("hosts"), dir.join("hosts")).unwrap();
    symlink(dir.join("inner.txt"), dir.join("ilink")).unwrap();

    let ws = sandbox(&dir).await;
    // The sandbox does not follow a link out of the project: it makes its own
    // /vendor, and cannot remove /hosts (the rm fails, the path is touched).
    run(
        &ws,
        &dir,
        "mkdir -p vendor && echo k >> vendor/authorized_keys",
    )
    .await;
    ws.fs()
        .write(&dir.join("vendor/new.txt"), b"evil\n")
        .await
        .unwrap();
    run(&ws, &dir, "rm hosts || true").await;
    run(&ws, &dir, "rm ilink && echo replaced > ilink").await;
    run(&ws, &dir, "echo new > ok.txt").await;

    let set = ws.changes().unwrap();
    let list = set.list().await;
    let through_a_link = |p: &str| p.starts_with("/vendor") || p == "/hosts" || p == "/ilink";
    // A link on the way is no conflict: nobody edited anything on disk.
    assert!(list.iter().all(|c| !c.conflict), "{list:?}");
    for p in ["/vendor/authorized_keys", "/vendor/new.txt", "/ilink"] {
        assert!(list.iter().any(|c| c.path == p), "{p} in {list:?}");
    }

    let report = set.apply().await.unwrap();
    let applied: Vec<_> = report.applied.iter().map(|c| c.path.as_str()).collect();
    assert_eq!(applied, ["/ok.txt"], "{report:?}");
    assert!(report.skipped.is_empty(), "{report:?}");
    assert!(
        report
            .failed
            .iter()
            .all(|f| through_a_link(&f.change.path) && f.code == ApplyFailureCode::OutsideProject),
        "{report:?}"
    );
    for p in ["/vendor/authorized_keys", "/vendor/new.txt", "/ilink"] {
        assert!(
            report.failed.iter().any(|f| f.change.path == p),
            "{p} in {report:?}"
        );
    }
    // Outside the project nothing moved; inside, the links are still links.
    assert_eq!(names(&outside), ["authorized_keys", "hosts"]);
    assert_eq!(
        fs::read_to_string(outside.join("authorized_keys")).unwrap(),
        "mine\n"
    );
    assert_eq!(
        fs::read_to_string(outside.join("hosts")).unwrap(),
        "127.0.0.1 localhost\n"
    );
    assert_eq!(
        fs::read_to_string(dir.join("inner.txt")).unwrap(),
        "inner\n"
    );
    for link in ["vendor", "hosts", "ilink"] {
        assert!(is_link(&dir.join(link)), "{link}");
    }
    assert_eq!(fs::read_to_string(dir.join("ok.txt")).unwrap(), "new\n");
    // Refused and still listed: the human decides.
    let left = set.list().await;
    assert!(
        !left.is_empty() && left.iter().all(|c| through_a_link(&c.path)),
        "{left:?}"
    );
}

#[tokio::test]
async fn a_directory_that_became_a_file_and_a_file_that_became_a_directory_are_applied() {
    let (_t, dir) = tempdir();
    fs::create_dir(dir.join("build")).unwrap();
    fs::write(dir.join("build/old.o"), "obj").unwrap();
    fs::write(dir.join("out"), "a file\n").unwrap();
    let ws = sandbox(&dir).await;
    run(
        &ws,
        &dir,
        "echo a > build/new.o && rm -rf build && echo x > build",
    )
    .await;
    run(&ws, &dir, "rm out && mkdir out && echo y > out/a").await;
    let report = ws.changes().unwrap().apply().await.unwrap();
    assert!(
        report.skipped.is_empty() && report.failed.is_empty(),
        "{report:?}"
    );
    assert_eq!(fs::read_to_string(dir.join("build")).unwrap(), "x\n");
    assert_eq!(fs::read_to_string(dir.join("out/a")).unwrap(), "y\n");
    assert!(ws.changes().unwrap().list().await.is_empty());
}

/// B38: what the disk refuses, and a link the sandbox made, are each reported
/// as failed with their code -- never as a conflict, never lost.
#[cfg(unix)]
#[tokio::test]
async fn a_change_that_cannot_be_applied_is_reported_and_the_others_still_are() {
    use std::os::unix::fs::PermissionsExt;
    let (_t, dir) = tempdir();
    fs::write(dir.join("ok.txt"), "old\n").unwrap();
    fs::create_dir(dir.join("locked")).unwrap();
    let ws = sandbox(&dir).await;
    run(
        &ws,
        &dir,
        "ln -s /etc/hosts l && echo x > locked/new.txt && echo new > ok.txt",
    )
    .await;
    let mode = |m| fs::Permissions::from_mode(m);
    fs::set_permissions(dir.join("locked"), mode(0o555)).unwrap();
    let set = ws.changes().unwrap();
    let report = set.apply().await;
    fs::set_permissions(dir.join("locked"), mode(0o755)).unwrap();
    let report = report.expect("one failed change is not a failed apply");
    let failed: Vec<_> = report
        .failed
        .iter()
        .map(|f| (f.change.path.as_str(), f.code))
        .collect();
    assert_eq!(
        failed,
        [
            ("/l", ApplyFailureCode::Symlink),
            ("/locked/new.txt", ApplyFailureCode::WriteFailed),
        ]
    );
    let applied: Vec<_> = report.applied.iter().map(|c| c.path.as_str()).collect();
    assert_eq!(applied, ["/ok.txt"]);
    assert!(report.skipped.is_empty(), "{report:?}");
    assert_eq!(fs::read_to_string(dir.join("ok.txt")).unwrap(), "new\n");
    // Still listed: nothing was lost.
    let left: Vec<_> = set.list().await.into_iter().map(|c| c.path).collect();
    assert_eq!(left, ["/l", "/locked/new.txt"]);
}
