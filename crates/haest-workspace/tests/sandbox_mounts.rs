//! The default sandbox's real mounts: what reaches the disk through them, and
//! what does not.

mod common;

use std::os::unix::fs::PermissionsExt;

use common::tempdir;
use haest_core::workspace::Workspace;
use haest_workspace::{SandboxMounts, SandboxWorkspace};

fn mode(path: &std::path::Path) -> u32 {
    std::fs::metadata(path).unwrap().permissions().mode() & 0o777
}

#[tokio::test]
async fn chmod_through_the_tools_reaches_a_writable_mount_on_disk() {
    let (_t, dir) = tempdir();
    let (_o, out) = tempdir();
    let file = out.join("log.jsonl");
    std::fs::write(&file, "x").unwrap();
    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o644)).unwrap();
    let ws = SandboxWorkspace::open(SandboxMounts::new(&dir).writable(&out))
        .await
        .unwrap();

    ws.fs().chmod(&file, 0o600).await.unwrap();
    assert_eq!(mode(&file), 0o600);
}
