//! The new file the apply puts in a file's place, on the real disk with a
//! fault in it: a full disk, a write the runtime is slow to start, a rename
//! nothing can do, a name the system refuses, a directory that passes its
//! group on.

mod faulty;

use std::{
    fs::{self, Metadata, Permissions},
    os::unix::fs::{MetadataExt, PermissionsExt},
    path::{Path, PathBuf},
    sync::{Arc, atomic::Ordering},
    time::Duration,
};

use faulty::{Fault, Faulty};
use haest_core::{
    fs::{FsRef, MemFs},
    workspace::{ApplyFailureCode, Change, ChangeSet},
};

use crate::{bridge::BashkitFsOverHaest, overlay::OverlayChanges};

const HUMAN: &str = "the human's version\n";
const SANDBOX: &str = "the sandbox's version\n";

/// A project dir by its real path, holding `name` with `human` in it, and
/// the sandbox's edit of it to `sandbox`, applied to the disk `fault` makes.
async fn edited(
    name: &str,
    human: &str,
    sandbox: &str,
    fault: Option<Fault>,
) -> (tempfile::TempDir, PathBuf, Arc<Faulty>, OverlayChanges) {
    let t = tempfile::tempdir().unwrap();
    let root = fs::canonicalize(t.path()).unwrap();
    let real = root.join(name);
    fs::write(&real, human).unwrap();
    let path = Path::new("/").join(name);
    let files = MemFs::from_files([(path.as_path(), human)]);
    let overlay = Arc::new(BashkitFsOverHaest::new(FsRef::new(files)));
    let disk = Faulty::new(&real, fault);
    let changes = OverlayChanges::new(overlay, disk.clone(), &root);
    let recorder = changes.recorder();
    recorder
        .write_file(&path, sandbox.as_bytes())
        .await
        .unwrap();
    (t, real, disk, changes)
}

fn conflicts(changes: &[Change]) -> Vec<(&str, bool)> {
    changes
        .iter()
        .map(|c| (c.path.as_str(), c.conflict))
        .collect()
}

fn names(dir: &Path) -> Vec<String> {
    let mut all: Vec<String> = fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    all.sort();
    all
}

/// Everything under `dir`, at any depth, without following a link.
fn under(dir: &Path) -> Vec<(PathBuf, Metadata)> {
    let mut all = Vec::new();
    for e in fs::read_dir(dir).unwrap() {
        let path = e.unwrap().path();
        let meta = fs::symlink_metadata(&path).unwrap();
        let deeper = meta.is_dir().then(|| under(&path));
        all.push((path, meta));
        all.extend(deeper.into_iter().flatten());
    }
    all
}

/// Neither the group nor anyone else may reach into it.
fn owner_only(meta: &Metadata) -> bool {
    meta.permissions().mode() & 0o077 == 0
}

/// The disk fills up while the apply writes the sandbox's `notes.txt`.
/// The write had emptied the human's file first: the change failed,
/// the human's text was gone, and since the disk then held neither
/// version, every later scan listed it as a conflict and every apply
/// skipped it.
#[tokio::test]
async fn a_write_the_full_disk_stops_leaves_the_humans_file_whole() {
    let human = "the human's notes\n".repeat(50);
    let sandbox = "the sandbox's longer notes\n".repeat(100);
    let (_t, real, disk, changes) = edited("notes.txt", &human, &sandbox, None).await;
    disk.room.store(64, Ordering::SeqCst);

    let report = changes.apply().await;
    let failed: Vec<_> = report
        .failed
        .iter()
        .map(|f| (f.change.path.as_str(), f.code))
        .collect();
    assert_eq!(failed, [("/notes.txt", ApplyFailureCode::WriteFailed)]);
    let on_disk = fs::read_to_string(&real).unwrap();
    assert!(
        on_disk == human,
        "the human's file: {} bytes",
        on_disk.len()
    );
    assert_eq!(
        names(real.parent().unwrap()),
        ["notes.txt"],
        "nothing left beside it"
    );
    assert_eq!(conflicts(&changes.list().await), [("/notes.txt", false)]);

    // Room is made: the next apply writes it.
    disk.room.store(usize::MAX, Ordering::SeqCst);
    let report = changes.apply().await;
    assert_eq!(conflicts(&report.applied), [("/notes.txt", false)]);
    let on_disk = fs::read_to_string(&real).unwrap();
    assert!(on_disk == sandbox, "the sandbox's: {} bytes", on_disk.len());
    assert!(changes.list().await.is_empty());
}

/// The apply is dropped (Ctrl+C at the haest command's `/apply`, a host's
/// timeout) while the runtime still holds the write of the new file's
/// bytes. The drop removed the new file, owner-only till then; the write
/// then made it again, open to everyone, with the sandbox's version of the
/// private `secret.env` in it, under a name its own ignore pattern missed.
#[tokio::test]
async fn an_apply_dropped_before_its_write_starts_leaves_no_copy_open_to_others() {
    let fault = Some(Fault::SlowWrite);
    let (_t, real, disk, changes) = edited("secret.env", HUMAN, SANDBOX, fault).await;
    fs::set_permissions(&real, Permissions::from_mode(0o600)).unwrap();

    let dropped = tokio::select! {
        biased;
        () = disk.began.notified() => true,
        _ = changes.apply() => false,
    };
    assert!(dropped, "the apply ended before its write began");
    // The write the runtime held lands, or finds nowhere to land.
    tokio::time::sleep(Duration::from_millis(300)).await;

    let root = real.parent().unwrap();
    for (path, meta) in under(root).iter().filter(|(p, _)| *p != real) {
        let shown = path.display();
        assert!(owner_only(meta), "{shown} is open to others: {meta:?}");
        if meta.is_file() && fs::read(path).unwrap() == SANDBOX.as_bytes() {
            let dir = path.parent().unwrap();
            let own = dir != root && owner_only(&fs::metadata(dir).unwrap());
            assert!(own, "{shown} holds the sandbox's version");
        }
    }
    assert_eq!(fs::read_to_string(&real).unwrap(), HUMAN);
}

/// The sandbox's edit of `name`, applied to the disk `fault` makes: on the
/// disk, nothing left beside it, listed no more.
async fn assert_applied(name: &str, fault: Fault) {
    let (_t, real, _disk, changes) = edited(name, HUMAN, SANDBOX, Some(fault)).await;
    let report = changes.apply().await;
    assert!(report.failed.is_empty(), "{report:?}");
    let path = format!("/{name}");
    assert_eq!(conflicts(&report.applied), [(path.as_str(), false)]);
    assert_eq!(fs::read_to_string(&real).unwrap(), SANDBOX);
    assert_eq!(names(real.parent().unwrap()), [name]);
    assert!(changes.list().await.is_empty());
}

/// `config.yml` is a mount point of its own (one file bind-mounted into the
/// project, as a dev container does): nothing can be renamed over it. Its
/// edit failed on every apply; it is written in place.
#[tokio::test]
async fn a_file_nothing_can_be_renamed_over_is_written_in_place() {
    assert_applied("config.yml", Fault::BusyRename).await;
}

/// `notes.txt`'s path is just short of the system's cap on a path, so the
/// longer name of a new file beside it is refused. Its edit failed on every
/// apply; it is written in place.
#[tokio::test]
async fn a_file_whose_new_name_the_system_refuses_is_written_in_place() {
    assert_applied("notes.txt", Fault::NewName(libc::ENAMETOOLONG)).await;
}

/// The disk has no room for even an empty new file beside `notes.txt`:
/// the change fails, and is not written in place, where the bytes would
/// not all go in either. The human's file stays whole, the change listed.
#[tokio::test]
async fn a_disk_with_no_room_for_a_new_name_fails_the_change() {
    let fault = Some(Fault::NewName(libc::ENOSPC));
    let (_t, real, _disk, changes) = edited("notes.txt", HUMAN, SANDBOX, fault).await;
    let report = changes.apply().await;
    let failed: Vec<_> = report
        .failed
        .iter()
        .map(|f| f.change.path.as_str())
        .collect();
    assert_eq!(failed, ["/notes.txt"], "{report:?}");
    assert_eq!(fs::read_to_string(&real).unwrap(), HUMAN);
    assert_eq!(names(real.parent().unwrap()), ["notes.txt"]);
    assert_eq!(conflicts(&changes.list().await), [("/notes.txt", false)]);
}

/// The sandbox's edit of `name` on the disk `fault` makes, applied: the
/// file is replaced, with its group and mode, and nothing is left beside
/// it. Returns the disk, which saw what the apply did.
async fn replaced(name: &str, fault: Option<Fault>) -> Arc<Faulty> {
    let (_t, real, disk, changes) = edited(name, HUMAN, SANDBOX, fault).await;
    fs::set_permissions(&real, Permissions::from_mode(0o640)).unwrap();
    let before = fs::metadata(&real).unwrap();
    let report = changes.apply().await;
    assert!(report.failed.is_empty(), "{report:?}");
    assert_eq!(fs::read_to_string(&real).unwrap(), SANDBOX);
    let after = fs::metadata(&real).unwrap();
    assert_ne!(after.ino(), before.ino(), "written in place, not replaced");
    assert_eq!(after.gid(), before.gid());
    assert_eq!(after.permissions().mode() & 0o7777, 0o640);
    assert_eq!(names(real.parent().unwrap()), [name]);
    disk
}

/// Each new file `disk` saw the sandbox's bytes go into was its owner's
/// alone by then.
fn filled_owner_only(disk: &Faulty) {
    let filled = disk.filled.lock().unwrap().clone();
    assert!(!filled.is_empty(), "no bytes went into a new file");
    for mode in filled {
        let own = mode.is_some_and(|m| m & 0o077 == 0);
        let mode = mode.map(|m| format!("{m:o}"));
        assert!(own, "the sandbox's bytes went into a file at {mode:?}");
    }
}

/// The sandbox's bytes went into the new file while its group and
/// everyone else could still open it (its directory kept them out).
#[tokio::test]
async fn a_new_file_is_its_owners_alone_before_the_sandboxs_bytes_go_in() {
    let disk = replaced("secret.env", None).await;
    filled_owner_only(&disk);
}

/// A team's directory passes its group on (setgid). The apply set the
/// mode of the new file's directory, and Linux drops the bit for a run not
/// in that group: the new file got the run's own group, so the edit was
/// written in place (cut short on a full disk). That directory now keeps
/// the mode it was made with, and the new file is its owner's alone before
/// anything goes in.
#[tokio::test]
async fn a_directory_that_passes_its_group_on_keeps_its_mode() {
    let disk = replaced("notes.txt", Some(Fault::Setgid)).await;
    let chmods = disk.setgid_chmods.lock().unwrap().clone();
    let chmods: Vec<_> = chmods.iter().map(|m| format!("{m:o}")).collect();
    assert!(chmods.is_empty(), "its mode was set: {chmods:?}");
    filled_owner_only(&disk);
}
