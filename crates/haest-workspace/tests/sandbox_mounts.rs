//! The default sandbox's real mounts: what reaches the disk through them, and
//! what does not.

mod common;

use std::{os::unix::fs::PermissionsExt, path::PathBuf};

use common::{opts, tempdir};
use haest_core::workspace::Workspace;
use haest_workspace::{SandboxMounts, SandboxWorkspace};

fn mode(path: &std::path::Path) -> u32 {
    std::fs::metadata(path).unwrap().permissions().mode() & 0o777
}

/// A temp dir by its real path and by a path relative to this process's
/// current directory (the crate's root), so a test names it relatively
/// without changing the process-wide current directory. It is made in the
/// target directory's scratch space (`target/tmp`, which `cargo clean`
/// empties), never in the crate: a run killed mid-test skips the cleanup.
/// The relative path climbs only to where the two paths part, so (with the
/// target directory in the workspace) it is not the real path again once a
/// mount table roots it at `/`, as the bug this file pins did.
fn tempdir_relative() -> (tempfile::TempDir, PathBuf, PathBuf) {
    let t = tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR")).unwrap();
    let real = std::fs::canonicalize(t.path()).unwrap();
    let here = std::env::current_dir().unwrap();
    let shared = here
        .components()
        .zip(real.components())
        .take_while(|(a, b)| a == b)
        .count();
    let mut rel: PathBuf = here.components().skip(shared).map(|_| "..").collect();
    rel.extend(real.components().skip(shared));
    (t, real, rel)
}

/// A relative project was mounted at `/` + its path, and the overlay applied
/// to the path as it was: "." put the project over the sandbox's root, the
/// tools found nothing at the project's real path, and scratch writes became
/// changes to apply.
#[tokio::test]
async fn a_relative_project_is_mounted_at_its_real_path() {
    let (_t, dir, rel) = tempdir_relative();
    std::fs::write(dir.join("a.txt"), "hello\n").unwrap();
    let ws = SandboxWorkspace::open(SandboxMounts::new(&rel))
        .await
        .unwrap();

    let real = dir.join("a.txt");
    assert_eq!(ws.fs().read_to_string(&real).await.unwrap(), "hello\n");
    let r = ws
        .shell(
            "cat a.txt && mkdir -p /tmp && echo x > /tmp/n.txt",
            opts(&dir),
        )
        .await;
    assert_eq!(r.stdout, "hello\n", "{r:?}");
    assert!(ws.changes().unwrap().list().await.is_empty());
}

/// A relative writable dir was mounted at `/` + its path, so a write to its
/// real path went to the scratch disk and never reached the disk.
#[tokio::test]
async fn a_relative_writable_dir_writes_through_at_its_real_path() {
    let (_t, dir) = tempdir();
    let (_o, out, rel) = tempdir_relative();
    let ws = SandboxWorkspace::open(SandboxMounts::new(&dir).with_writable(&rel))
        .await
        .unwrap();

    let kept = out.join("kept.txt");
    ws.fs().write(&kept, b"kept").await.unwrap();
    assert_eq!(std::fs::read_to_string(&kept).unwrap(), "kept");
}

#[tokio::test]
async fn a_writable_mount_goes_through_to_the_disk_and_a_readable_one_is_read_only() {
    let (_t, dir) = tempdir();
    let (_o, out) = tempdir();
    let (_s, skills) = tempdir();
    std::fs::write(skills.join("SKILL.md"), "skill\n").unwrap();
    let ws = SandboxWorkspace::open(
        SandboxMounts::new(&dir)
            .with_writable(&out)
            .with_readable(&skills),
    )
    .await
    .unwrap();

    ws.fs().write(&out.join("kept.txt"), b"kept").await.unwrap();
    assert_eq!(
        std::fs::read_to_string(out.join("kept.txt")).unwrap(),
        "kept"
    );

    let read = ws
        .shell(&format!("cat {}/SKILL.md", skills.display()), opts(&dir))
        .await;
    assert_eq!(read.stdout, "skill\n");
    let write = ws
        .shell(&format!("echo x > {}/new.md", skills.display()), opts(&dir))
        .await;
    assert_ne!(write.exit_code, 0);
    assert!(!skills.join("new.md").exists());
}

#[tokio::test]
async fn chmod_through_the_tools_reaches_a_writable_mount_on_disk() {
    let (_t, dir) = tempdir();
    let (_o, out) = tempdir();
    let file = out.join("log.jsonl");
    std::fs::write(&file, "x").unwrap();
    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o644)).unwrap();
    let ws = SandboxWorkspace::open(SandboxMounts::new(&dir).with_writable(&out))
        .await
        .unwrap();

    ws.fs().chmod(&file, 0o600).await.unwrap();
    assert_eq!(mode(&file), 0o600);
}

#[tokio::test]
async fn a_writable_dir_inside_the_project_writes_through_to_the_disk() {
    let (_t, dir) = tempdir();
    let spill = dir.join("spill");
    std::fs::create_dir(&spill).unwrap();
    let ws = SandboxWorkspace::open(SandboxMounts::new(&dir).with_writable(&spill))
        .await
        .unwrap();

    let r = ws
        .shell("echo kept > spill/a.txt && echo held > b.txt", opts(&dir))
        .await;
    assert_eq!(r.exit_code, 0, "{r:?}");
    // The deeper mount owns its subtree; the rest of the project stays copy-on-write.
    assert_eq!(
        std::fs::read_to_string(spill.join("a.txt")).unwrap(),
        "kept\n"
    );
    assert!(!dir.join("b.txt").exists());
}

/// A sandbox that cannot open says which dir: the project, or a mount.
#[tokio::test]
async fn a_dir_that_does_not_open_is_named_in_the_error() {
    let (_t, dir) = tempdir();
    let missing = dir.join("no-such-dir");
    for mounts in [
        SandboxMounts::new(&missing),
        SandboxMounts::new(&dir).with_readable(&missing),
        SandboxMounts::new(&dir).with_writable(&missing),
    ] {
        let err = SandboxWorkspace::open(mounts.clone())
            .await
            .expect_err("the dir does not exist");
        let shown = err.to_string();
        assert!(
            shown.contains(missing.to_str().unwrap()),
            "{mounts:?}: {shown}"
        );
    }
}

#[tokio::test]
async fn a_readable_dir_around_the_project_shows_its_other_files() {
    let (_t, home) = tempdir();
    let dir = home.join("project");
    std::fs::create_dir(&dir).unwrap();
    std::fs::write(home.join("notes.md"), "around\n").unwrap();
    let ws = SandboxWorkspace::open(SandboxMounts::new(&dir).with_readable(&home))
        .await
        .unwrap();

    let r = ws
        .shell("cat ../notes.md && echo x > mine.txt", opts(&dir))
        .await;
    assert_eq!(r.stdout, "around\n", "{r:?}");
    // The project inside it is still the overlay: writable, in memory.
    assert_eq!(r.exit_code, 0, "{r:?}");
    assert!(!dir.join("mine.txt").exists());
}
