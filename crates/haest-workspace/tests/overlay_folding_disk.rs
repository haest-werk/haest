//! The renames by case of `overlay_names_as_one.rs`, on a disk that reads
//! two names of a file as one and lists the one it holds: exFAT and FAT on
//! macOS, vfat on Linux. Opt-in: `HAEST_TEST_FOLDING_DIR` names a directory
//! on such a disk; unset, each test passes without running. On macOS,
//! `HAEST_TEST_FOLDING_REMOUNT` is a command that makes the disk drop the
//! names it cached (detach and attach its image), run between the human's
//! files and the sandbox.
//!
//! There `realpath` answers with the name a file was first looked up by
//! since its name was cached (macOS), or with the one asked (Linux), never
//! with the one the directory lists. Nothing is lost: what would land on
//! the other name, or take what the sandbox shows under it, is held as a
//! conflict, and the disk keeps the human's files as they were.
//!
//! `HAEST_TEST_FOLDING_ROOT` names the root of such a volume, where one
//! test writes `old.txt` and `notes.md` and removes them after; unset, it
//! passes without running.

mod common;

use std::{fs, path::PathBuf, time::Duration};

use common::{
    disk::{
        DIRECTORY, Disk, NUMBERED, RENAME_AND_COPY, RENAME_THEN_EDIT, RENAME_TO_LOWER,
        RENAME_TO_LOWER_THEN_EDIT, RENAME_WITH_NO_ASCII_LETTER, Renamed, copy_by_case_then_edit,
    },
    project::{apply_all, changes, contents, nothing_lost, pairs, reads_as_one, run},
};
use haest_core::workspace::Workspace;
use tokio::sync::Mutex;

/// Held by each test: one remounts the disk the others are on.
static DISK: Mutex<()> = Mutex::const_new(());

async fn on_the_folding_disk(case: &Renamed) {
    let _disk = DISK.lock().await;
    if let Some(disk) = Disk::folding() {
        case.check(&disk).await;
    }
}

#[tokio::test]
async fn a_rename_by_case_and_a_copy_lose_nothing() {
    on_the_folding_disk(&RENAME_AND_COPY).await;
}

#[tokio::test]
async fn a_rename_to_lower_case_loses_nothing() {
    on_the_folding_disk(&RENAME_TO_LOWER).await;
}

#[tokio::test]
async fn a_rename_by_case_then_an_edit_loses_nothing() {
    on_the_folding_disk(&RENAME_THEN_EDIT).await;
}

#[tokio::test]
async fn a_rename_to_lower_case_then_an_edit_loses_nothing() {
    on_the_folding_disk(&RENAME_TO_LOWER_THEN_EDIT).await;
}

#[tokio::test]
async fn a_directory_renamed_by_case_loses_nothing() {
    on_the_folding_disk(&DIRECTORY).await;
}

/// A name with no ASCII letter: a Greek name renamed by case loses
/// nothing, and numbered files take their edits.
#[tokio::test]
async fn names_with_no_ascii_letter_lose_nothing_and_take_their_edits() {
    on_the_folding_disk(&RENAME_WITH_NO_ASCII_LETTER).await;
    on_the_folding_disk(&NUMBERED).await;
}

/// The copy lands on the human's file: a conflict, and the file as it was.
#[tokio::test]
async fn a_copy_by_case_then_an_edit_leaves_the_humans_file_as_it_was() {
    let _disk = DISK.lock().await;
    if let Some(disk) = Disk::folding() {
        copy_by_case_then_edit(&disk).await;
    }
}

/// A delete of a name the disk finds under another, and an overwrite of a
/// file the sandbox also shows under another name, empty or not (exFAT and
/// FAT give an empty file one made-up inode number per `lstat`, the same
/// under both names): each held, the human's file as it was.
#[tokio::test]
async fn a_delete_or_an_overwrite_under_another_name_takes_nothing() {
    const HUMAN: &str = "the human's\n";
    let _disk = DISK.lock().await;
    for text in [HUMAN, ""] {
        for command in [
            "rm readme.md",
            "cp README.md readme.md && echo new > README.md",
        ] {
            let Some(disk) = Disk::folding() else {
                return;
            };
            disk.make(&[("README.md", text)]);
            let one = reads_as_one(&disk.dir, "README.md", "readme.md");
            let ws = disk.sandbox().await;
            let r = ws.shell(command, common::opts(&disk.dir)).await;
            if !one {
                // A disk that tells the names apart: no readme.md to
                // delete, and the copy is a file of its own.
                if command.starts_with("rm ") {
                    assert_ne!(r.exit_code, 0, "{command}: {r:?}");
                } else {
                    assert_eq!(r.exit_code, 0, "{command}: {r:?}");
                    apply_all(&ws).await;
                    let want = [("README.md", "new\n"), ("readme.md", text)];
                    assert_eq!(contents(&disk.dir), pairs(&want), "{command}");
                }
                continue;
            }
            assert_eq!(r.exit_code, 0, "{command}: {r:?}");
            let report = changes(&ws).apply().await;
            nothing_lost(&ws, &disk.dir, &report).await;
            assert!(report.applied.is_empty(), "{command}: {report:?}");
            let kept = contents(&disk.dir);
            assert_eq!(kept, pairs(&[("README.md", text)]), "{command}: {report:?}");
        }
    }
}

/// Files a test made, removed when it ends, however it ends.
struct Made(Vec<PathBuf>);

impl Drop for Made {
    fn drop(&mut self) {
        for path in &self.0 {
            let _ = fs::remove_file(path);
        }
    }
}

/// The root of an exFAT, FAT or vfat volume never changes its ctime: a
/// listing kept by that stamp read a file the human made there after the
/// model first looked as absent, so Write told the model it created the
/// file, and applying replaced the human's.
#[tokio::test]
async fn a_file_the_human_makes_at_a_volume_root_is_seen() {
    let _disk = DISK.lock().await;
    let Some(root) = std::env::var_os("HAEST_TEST_FOLDING_ROOT") else {
        return;
    };
    let root = fs::canonicalize(root).unwrap();
    let (old, notes) = (root.join("old.txt"), root.join("notes.md"));
    let _made = Made(vec![old.clone(), notes.clone()]);
    fs::write(&old, "old\n").unwrap();
    // Each time past the time a listing waits for its directory to settle:
    // FAT on macOS stamps its root with the time it was mounted.
    let settle = Duration::from_millis(2200);
    tokio::time::sleep(settle).await;
    let ws = common::sandbox(&root).await;
    run(&ws, &root, "cat old.txt").await;
    tokio::time::sleep(settle).await;
    fs::write(&notes, "the human's\n").unwrap();
    // What Write asks to say "Created" or "Overwrote".
    assert!(ws.fs().exists(&notes).await, "notes.md reads as absent");
    let shown = ws.fs().read(&notes).await.unwrap();
    assert_eq!(shown, b"the human's\n");
}
