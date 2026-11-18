//! The scan: which paths it compares, and what that costs.

use std::{path::Path, sync::Arc};

use haest_core::{
    fs::{Fs, FsRef},
    workspace::ChangeSet,
};

use super::{OverlayChanges, testing::Disk};
use crate::bridge::BashkitFsOverHaest;

/// A tree whose every directory the sandbox made (`mkdir a a/b a/b/c`):
/// the scan walked the tree of each one it found new, so the top was
/// listed three times over, and the deeper, the more.
#[tokio::test]
async fn a_new_tree_is_walked_once_per_scan() {
    let disk = Arc::new(Disk::default());
    disk.inner.create_dir_all(Path::new("/p")).await.unwrap();
    let files = Arc::new(Disk::default());
    let overlay = Arc::new(BashkitFsOverHaest::new(FsRef::from(files.clone())));
    let changes = OverlayChanges::new(overlay, disk, "/p");
    let recorder = changes.recorder();
    for dir in ["/a", "/a/b", "/a/b/c"] {
        recorder.mkdir(Path::new(dir), false).await.unwrap();
    }
    recorder
        .write_file(Path::new("/a/b/c/f"), b"x")
        .await
        .unwrap();
    files.listed.lock().unwrap().clear();

    let listed: Vec<String> = changes.list().await.into_iter().map(|c| c.path).collect();
    assert_eq!(listed, ["/a", "/a/b", "/a/b/c", "/a/b/c/f"]);
    let walked = files.listed.lock().unwrap();
    assert_eq!(
        *walked,
        ["/a", "/a/b", "/a/b/c"].map(Path::new),
        "{walked:?}"
    );
}
