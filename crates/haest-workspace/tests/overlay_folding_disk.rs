//! The renames by case of `overlay_spelling.rs`, on a disk that reads two
//! spellings of a name as one and lists the one it holds: exFAT and FAT on
//! macOS, vfat on Linux. Opt-in: `HAEST_TEST_FOLDING_DIR` names a directory
//! on such a disk; unset, each test passes without running. On macOS,
//! `HAEST_TEST_FOLDING_REMOUNT` is a command that makes the disk drop the
//! names it cached (detach and attach its image), run between the human's
//! files and the sandbox.
//!
//! There `realpath` answers with the spelling a file was first looked up by
//! since its name was cached (macOS), or with the one asked (Linux), never
//! with the one the directory lists. The sandbox took that answer for the
//! disk's spelling: `mv README.md readme.md` read `readme.md` as a file the
//! disk held, and applying deleted `README.md`, the only file there was.
//!
//! `HAEST_TEST_FOLDING_ROOT` names the root of such a volume, where one
//! test writes `old.txt` and `notes.md` and removes them after; unset, it
//! passes without running.

mod common;

use std::{fs, path::PathBuf, time::Duration};

use common::spelling::{
    disk::{
        DIRECTORY, Disk, NUMBERED, RENAME_AND_COPY, RENAME_THEN_EDIT, RENAME_TO_LOWER,
        RENAME_TO_LOWER_THEN_EDIT, RENAME_WITH_NO_ASCII_LETTER, Renamed, copy_by_case_then_edit,
    },
    run,
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
async fn a_rename_by_case_and_a_copy_keep_both_files() {
    on_the_folding_disk(&RENAME_AND_COPY).await;
}

#[tokio::test]
async fn a_rename_to_lower_case_keeps_the_file() {
    on_the_folding_disk(&RENAME_TO_LOWER).await;
}

#[tokio::test]
async fn a_rename_by_case_then_an_edit_keeps_the_edit() {
    on_the_folding_disk(&RENAME_THEN_EDIT).await;
}

#[tokio::test]
async fn a_rename_to_lower_case_then_an_edit_keeps_the_edit() {
    on_the_folding_disk(&RENAME_TO_LOWER_THEN_EDIT).await;
}

#[tokio::test]
async fn a_directory_renamed_by_case_keeps_its_files() {
    on_the_folding_disk(&DIRECTORY).await;
}

/// A name with no ASCII letter has no case to ask it by, and the disk's
/// listing still decides it: a Greek name renamed by case keeps its file,
/// and numbered files take their edits.
#[tokio::test]
async fn names_with_no_ascii_letter_keep_their_files_and_edits() {
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

/// Files a test made, removed when it ends, however it ends.
struct Made(Vec<PathBuf>);

impl Drop for Made {
    fn drop(&mut self) {
        for path in &self.0 {
            let _ = fs::remove_file(path);
        }
    }
}

/// The root of an exFAT, FAT or vfat volume never changes its ctime, and
/// the sandbox kept its first listing of it for good: a file the human
/// made there after the model first looked read as absent, so Write told
/// the model it created the file, and applying replaced the human's.
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
