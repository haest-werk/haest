//! The default sandbox's real mounts: what reaches the disk through them, and
//! what does not.

mod common;

use std::os::unix::fs::PermissionsExt;

use common::{opts, tempdir};
use haest_core::workspace::Workspace;
use haest_workspace::{SandboxMounts, SandboxWorkspace};

fn mode(path: &std::path::Path) -> u32 {
    std::fs::metadata(path).unwrap().permissions().mode() & 0o777
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
