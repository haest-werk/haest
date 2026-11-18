//! What applying may not do to the real disk, whatever the sandbox wrote:
//! reach through a symlink in the project, out of it or anywhere else, or
//! take a file the human saved along with a directory it removes. And what
//! an apply dropped partway may not do: forget a change it did not write.

mod common;

use std::{fs, path::Path};

use common::{
    drop_apply_partway, opts, sandbox,
    spelling::{apply_all, changes, contents, paths},
    tempdir,
};
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

    let report = set.apply().await;
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

/// On a disk that matches names without case or normalization (macOS's
/// default), `Vendor` and `cafe\u{301}` are the links `vendor` and
/// `caf\u{e9}`. A lookup in the parent's listing missed both, and apply
/// wrote through them. A write into a name the disk holds under another
/// spelling is a conflict, so it is skipped before any link is asked about
/// (the link lookup by the change's own spelling is `Links`' unit test). On
/// a disk that tells the spellings apart they are new directories in the
/// project.
#[tokio::test]
async fn a_link_spelled_another_way_is_never_written_through() {
    use std::os::unix::fs::symlink;
    let (_t, dir) = tempdir();
    let (_o, outside) = tempdir();
    symlink(&outside, dir.join("vendor")).unwrap();
    symlink(&outside, dir.join("caf\u{e9}")).unwrap();
    // Each change, and whether this disk reads its spelling as the link.
    let spelled = [
        ("/Vendor/a", is_link(&dir.join("Vendor"))),
        ("/cafe\u{301}/b", is_link(&dir.join("cafe\u{301}"))),
    ];
    let ws = sandbox(&dir).await;
    run(
        &ws,
        &dir,
        "mkdir -p Vendor cafe\u{301} && echo x > Vendor/a && echo x > cafe\u{301}/b",
    )
    .await;
    let report = ws.changes().unwrap().apply().await;
    let escaped = names(&outside);
    assert!(escaped.is_empty(), "outside holds {escaped:?}: {report:?}");
    for (path, through_a_link) in spelled {
        let skipped = report.skipped.iter().any(|c| c.path == path && c.conflict);
        let applied = report.applied.iter().any(|c| c.path == path);
        assert!(
            if through_a_link { skipped } else { applied },
            "{path}, a link on this disk: {through_a_link}: {report:?}"
        );
    }
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
    let report = ws.changes().unwrap().apply().await;
    assert!(
        report.skipped.is_empty() && report.failed.is_empty(),
        "{report:?}"
    );
    assert_eq!(fs::read_to_string(dir.join("build")).unwrap(), "x\n");
    assert_eq!(fs::read_to_string(dir.join("out/a")).unwrap(), "y\n");
    assert!(ws.changes().unwrap().list().await.is_empty());
}

/// The sandbox edits `lib/a.rs`, then removes `lib` whole (`rm -r`, `mv`, a
/// file in its place); the human saves `lib/a.rs` before the apply. The
/// report said `/lib/a.rs` was skipped as a conflict, but the delete of
/// `/lib` had taken the save along, and nothing was listed afterwards.
#[tokio::test]
async fn a_conflict_inside_a_directory_the_sandbox_removes_is_never_deleted() {
    for command in [
        "echo x > lib/a.rs && rm -r lib",
        "echo x > lib/a.rs && mv lib src",
        "echo x > lib/a.rs && rm -r lib && echo f > lib",
    ] {
        let (_t, dir) = tempdir();
        fs::create_dir(dir.join("lib")).unwrap();
        fs::write(dir.join("lib/a.rs"), "a\n").unwrap();
        fs::write(dir.join("lib/b.rs"), "b\n").unwrap();
        let ws = sandbox(&dir).await;
        run(&ws, &dir, command).await;
        fs::write(dir.join("lib/a.rs"), "human\n").unwrap();
        let report = changes(&ws).apply().await;
        let human = ("lib/a.rs".to_owned(), "human\n".to_owned());
        assert!(
            contents(&dir).contains(&human),
            "{command}: {:?}, {report:?}",
            contents(&dir)
        );
        assert!(report.failed.is_empty(), "{command}: {report:?}");
        // What waits is a conflict, and stays listed until the human settles it.
        let left = changes(&ws).list().await;
        assert!(paths(&left).contains(&"/lib/a.rs".to_owned()), "{command}");
        assert_eq!(paths(&left), paths(&report.skipped), "{command}");
        assert!(left.iter().all(|c| c.conflict), "{command}: {left:?}");
    }
}

/// B38: what the disk refuses, and a link the sandbox made, are each reported
/// as failed with their code -- never as a conflict, never lost.
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

/// The haest command drops `/apply` 2 s after Ctrl+C. After `mv src gen`,
/// what is under `gen` was found by walking it, so once `gen` was on the
/// disk, a drop left each file not yet written listed nowhere, with `src`
/// already deleted: the files were in the sandbox alone, and gone when the
/// session ended.
#[tokio::test]
async fn an_apply_dropped_partway_forgets_nothing() {
    let (_t, dir) = tempdir();
    fs::create_dir_all(dir.join("src/sub")).unwrap();
    fs::write(dir.join("src/sub/g"), "g\n").unwrap();
    let mut want = vec![("gen/sub/g".to_owned(), "g\n".to_owned())];
    for i in 0..20 {
        fs::write(dir.join(format!("src/f{i:02}")), format!("{i}\n")).unwrap();
        want.push((format!("gen/f{i:02}"), format!("{i}\n")));
    }
    want.sort();
    let ws = sandbox(&dir).await;
    run(&ws, &dir, "mv src gen").await;
    let shown = ws.shell("find gen -type f", opts(&dir)).await.stdout;
    assert_eq!(shown.lines().count(), want.len(), "{shown}");
    let gen_dir = dir.join("gen");
    drop_apply_partway(&changes(&ws), || {
        fs::read_dir(&gen_dir).is_ok_and(|mut d| d.next().is_some())
    })
    .await;
    // Each file the sandbox shows is on the disk or still listed.
    let listed = paths(&changes(&ws).list().await);
    let lost: Vec<&str> = shown
        .lines()
        .filter(|f| !dir.join(f).exists() && !listed.contains(&format!("/{f}")))
        .collect();
    assert!(lost.is_empty(), "lost {lost:?}, listed {listed:?}");
    apply_all(&ws).await;
    assert!(!dir.join("src").exists());
    assert_eq!(contents(&dir), want);
}
