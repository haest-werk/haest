//! Applying while the human works on the disk: a save or a delete that
//! lands between the scan and the write.

use std::{path::Path, sync::Arc};

use haest_core::{
    fs::{Fs, FsRef, MemFs},
    workspace::{Change, ChangeSet},
};

use crate::{
    bridge::BashkitFsOverHaest,
    overlay::{OverlayChanges, testing::Disk},
};

/// The project `/p` on `disk`, holding `a.md` and `b.md`, and the changes
/// of a sandbox over it.
async fn project(disk: &Arc<Disk>) -> OverlayChanges {
    let mem = MemFs::new();
    disk.inner.create_dir_all(Path::new("/p")).await.unwrap();
    for (name, body) in [("a.md", "a1\n"), ("b.md", "b1\n")] {
        let real = Path::new("/p").join(name);
        disk.inner.write(&real, body.as_bytes()).await.unwrap();
        mem.write(&Path::new("/").join(name), body.as_bytes())
            .await
            .unwrap();
    }
    let overlay = Arc::new(BashkitFsOverHaest::new(FsRef::new(mem)));
    OverlayChanges::new(overlay, disk.clone(), "/p")
}

/// Each change's path and whether it is a conflict.
fn conflicts(changes: &[Change]) -> Vec<(&str, bool)> {
    changes
        .iter()
        .map(|c| (c.path.as_str(), c.conflict))
        .collect()
}

async fn on_disk(disk: &Disk, path: &str) -> Option<String> {
    disk.inner.read_to_string(Path::new(path)).await.ok()
}

const HUMAN: &str = "the human was here\n";

/// The human saves `b.md` after the apply scanned it, before its write.
/// The sandbox's version was written over the human's, and reported
/// applied.
#[tokio::test]
async fn a_file_the_human_saves_during_the_apply_is_skipped_not_written_over() {
    let disk = Arc::new(Disk::default());
    let changes = project(&disk).await;
    let recorder = changes.recorder();
    recorder
        .write_file(Path::new("/a.md"), b"a2\n")
        .await
        .unwrap();
    recorder
        .write_file(Path::new("/b.md"), b"b2\n")
        .await
        .unwrap();
    disk.edit_on_read("/p/b.md", HUMAN.as_bytes());

    let report = changes.apply().await;
    assert_eq!(conflicts(&report.applied), [("/a.md", false)], "{report:?}");
    assert_eq!(conflicts(&report.skipped), [("/b.md", true)], "{report:?}");
    assert_eq!(on_disk(&disk, "/p/a.md").await.as_deref(), Some("a2\n"));
    assert_eq!(on_disk(&disk, "/p/b.md").await.as_deref(), Some(HUMAN));
    // Still listed, as a conflict: the human decides.
    assert_eq!(conflicts(&changes.list().await), [("/b.md", true)]);
}

/// The human deletes `b.md` after the apply scanned it, while the apply
/// deletes `a.md`. Taken for this apply's own doing, `b.md` was written
/// back as an add.
#[tokio::test]
async fn a_file_the_human_deletes_during_the_apply_is_skipped_not_written_back() {
    let disk = Arc::new(Disk::default());
    let changes = project(&disk).await;
    let recorder = changes.recorder();
    recorder.remove(Path::new("/a.md"), false).await.unwrap();
    recorder
        .write_file(Path::new("/b.md"), b"b2\n")
        .await
        .unwrap();
    disk.delete_on_read("/p/b.md");

    let report = changes.apply().await;
    assert_eq!(conflicts(&report.applied), [("/a.md", false)], "{report:?}");
    assert_eq!(conflicts(&report.skipped), [("/b.md", true)], "{report:?}");
    assert_eq!(on_disk(&disk, "/p/a.md").await, None);
    assert_eq!(on_disk(&disk, "/p/b.md").await, None);
    assert_eq!(conflicts(&changes.list().await), [("/b.md", true)]);
}

/// The same for a delete: the human's save was deleted.
#[tokio::test]
async fn a_file_the_human_saves_during_the_apply_is_skipped_not_deleted() {
    let disk = Arc::new(Disk::default());
    let changes = project(&disk).await;
    let recorder = changes.recorder();
    recorder.remove(Path::new("/a.md"), false).await.unwrap();
    recorder.remove(Path::new("/b.md"), false).await.unwrap();
    disk.edit_on_read("/p/b.md", HUMAN.as_bytes());

    let report = changes.apply().await;
    assert_eq!(conflicts(&report.applied), [("/a.md", false)], "{report:?}");
    assert_eq!(conflicts(&report.skipped), [("/b.md", true)], "{report:?}");
    assert_eq!(on_disk(&disk, "/p/a.md").await, None);
    assert_eq!(on_disk(&disk, "/p/b.md").await.as_deref(), Some(HUMAN));
    assert_eq!(conflicts(&changes.list().await), [("/b.md", true)]);
}
