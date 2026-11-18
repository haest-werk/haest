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

/// The sandbox wipes `lib` again while the apply runs (`rm -r lib && mkdir
/// lib`), over a file the human made there meanwhile. The apply forgot `lib`
/// as wiped all the same, so no list looked inside it again: the file was
/// gone in the sandbox, stayed on the disk, and was never listed. (The wipe
/// now waits for the apply to end.)
#[tokio::test]
async fn a_directory_wiped_again_while_the_apply_runs_is_still_compared() {
    let disk = Arc::new(Disk::default());
    let changes = project(&disk).await;
    disk.inner
        .create_dir_all(Path::new("/p/lib"))
        .await
        .unwrap();
    disk.inner
        .write(Path::new("/p/lib/a.rs"), b"a\n")
        .await
        .unwrap();
    // The sandbox's view, written past the recorder: what it reads there.
    let view = changes.overlay.inner().clone();
    view.mkdir(Path::new("/lib"), false).await.unwrap();
    view.write_file(Path::new("/lib/a.rs"), b"a\n")
        .await
        .unwrap();
    let recorder = changes.recorder();
    recorder.remove(Path::new("/lib"), true).await.unwrap();
    recorder.mkdir(Path::new("/lib"), false).await.unwrap();
    // Once the apply has deleted lib/a.rs: the human makes lib/h.rs, the
    // sandbox reads it, and wipes lib again.
    let human = disk.clone();
    let sandbox = (view, recorder);
    let (tx, rx) = std::sync::mpsc::channel();
    disk.after_remove(
        "/p/lib/a.rs",
        Box::new(move || {
            Box::pin(async move {
                let h = Path::new("/lib/h.rs");
                human
                    .inner
                    .write(Path::new("/p/lib/h.rs"), b"h\n")
                    .await
                    .unwrap();
                let (view, recorder) = sandbox;
                view.write_file(h, b"h\n").await.unwrap();
                // The sandbox's own work, as a job runs it: it waits for
                // the apply.
                let wipe = tokio::spawn(async move {
                    recorder.remove(Path::new("/lib"), true).await.unwrap();
                    recorder.mkdir(Path::new("/lib"), false).await.unwrap();
                });
                tx.send(wipe).unwrap();
            })
        }),
    );

    let report = changes.apply().await;
    rx.recv().unwrap().await.unwrap();
    assert_eq!(
        conflicts(&report.applied),
        [("/lib/a.rs", false)],
        "{report:?}"
    );
    assert_eq!(conflicts(&changes.list().await), [("/lib/h.rs", false)]);
    changes.apply().await;
    assert_eq!(on_disk(&disk, "/p/lib/h.rs").await, None);
}

/// The sandbox edits `lib/a.rs`, then removes `lib` whole; the human saves
/// `lib/a.rs` once the apply has scanned it. The delete of `lib` took the
/// save along, and `/lib/a.rs`'s delete then found it gone and was
/// reported applied.
#[tokio::test]
async fn a_save_the_human_makes_during_the_apply_inside_a_removed_directory_is_kept() {
    let disk = Arc::new(Disk::default());
    let changes = project(&disk).await;
    disk.inner
        .create_dir_all(Path::new("/p/lib"))
        .await
        .unwrap();
    disk.inner
        .write(Path::new("/p/lib/a.rs"), b"a\n")
        .await
        .unwrap();
    let view = changes.overlay.inner().clone();
    view.mkdir(Path::new("/lib"), false).await.unwrap();
    view.write_file(Path::new("/lib/a.rs"), b"a\n")
        .await
        .unwrap();
    let recorder = changes.recorder();
    recorder
        .write_file(Path::new("/lib/a.rs"), b"x\n")
        .await
        .unwrap();
    recorder.remove(Path::new("/lib"), true).await.unwrap();
    disk.edit_on_read("/p/lib/a.rs", HUMAN.as_bytes());

    let report = changes.apply().await;
    let both = [("/lib", true), ("/lib/a.rs", true)];
    assert_eq!(conflicts(&report.skipped), both, "{report:?}");
    assert_eq!(on_disk(&disk, "/p/lib/a.rs").await.as_deref(), Some(HUMAN));
    assert_eq!(conflicts(&changes.list().await), both);
}

/// The sandbox writes `a.md` again while the apply deletes it (a job that
/// runs while the human applies). The apply then forgot the path: the
/// sandbox held the new file, the disk nothing, and no list showed it.
#[tokio::test]
async fn a_sandbox_write_during_the_apply_is_still_listed() {
    let disk = Arc::new(Disk::default());
    let changes = project(&disk).await;
    let recorder = changes.recorder();
    recorder.remove(Path::new("/a.md"), false).await.unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    let sandbox = recorder.clone();
    disk.after_remove(
        "/p/a.md",
        Box::new(move || {
            Box::pin(async move {
                let mut job = tokio::spawn(async move {
                    sandbox
                        .write_file(Path::new("/a.md"), b"a3\n")
                        .await
                        .unwrap();
                });
                // It lands now, or it waits for the apply.
                let wait = std::time::Duration::from_millis(100);
                let landed = tokio::time::timeout(wait, &mut job).await;
                tx.send(landed.is_err().then_some(job)).unwrap();
            })
        }),
    );

    let report = changes.apply().await;
    if let Some(job) = rx.recv().unwrap() {
        job.await.unwrap();
    }
    assert_eq!(conflicts(&report.applied), [("/a.md", false)], "{report:?}");
    let shown = changes.overlay.inner().read_file(Path::new("/a.md")).await;
    assert_eq!(shown.unwrap(), b"a3\n");
    assert_eq!(conflicts(&changes.list().await), [("/a.md", false)]);
}
