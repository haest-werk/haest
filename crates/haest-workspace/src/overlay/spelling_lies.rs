//! The apply when the sandbox's answer to how the disk spells a name is
//! wrong: it loses nothing. The answer has been wrong in four ways so far: a
//! disk's kind kept past a disk swap (R27C-01), a disk that reads both
//! Unicode forms of a name as one where the case-swap probe said it tells
//! names apart (R27C-02, R27C-03), and a listing kept for a directory whose
//! stamp never moves (R27C-04). Here [`Spelling::told`] makes it wrong on
//! purpose, the way each of those did.

use std::{
    fs,
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering},
};

use haest_core::workspace::{ExecOptions, Workspace};

use crate::{SandboxWorkspace, spelling::Spelled};

const HUMAN: &str = "the human's\n";

/// A project directory, by its real path: on the disk
/// `HAEST_TEST_FOLDING_DIR` names where set (exFAT, FAT, vfat), else on the
/// temp disk.
fn project() -> (tempfile::TempDir, PathBuf) {
    let t = match std::env::var_os("HAEST_TEST_FOLDING_DIR") {
        Some(base) => tempfile::tempdir_in(base),
        None => tempfile::tempdir(),
    };
    let t = t.unwrap();
    let dir = fs::canonicalize(t.path()).unwrap();
    (t, dir)
}

async fn run(ws: &SandboxWorkspace, dir: &Path, command: &str) {
    let opts = ExecOptions::new(dir, std::time::Duration::from_secs(10));
    let r = ws.shell(command, opts).await;
    assert_eq!(r.exit_code, 0, "{command}: {r:?}");
}

/// What each file on the disk under `dir` holds, by its name as listed;
/// not macOS's `._<name>` files on a disk with no extended attributes.
fn held(dir: &Path) -> Vec<(String, String)> {
    let mut out: Vec<_> = fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| !n.starts_with("._"))
        .map(|n| {
            let text = fs::read_to_string(dir.join(&n)).unwrap();
            (n, text)
        })
        .collect();
    out.sort();
    out
}

/// What the files on the disk under `dir` hold, whatever their names.
fn texts(dir: &Path) -> Vec<String> {
    held(dir).into_iter().map(|(_, text)| text).collect()
}

/// Renames from the human's name to one the disk may read as the same:
/// by case, and from Unicode's decomposed form to its composed one, with
/// no ASCII letter and with one.
const RENAMES: [(&str, &str); 3] = [
    ("README.md", "readme.md"),
    ("\u{1112}\u{1161}\u{11ab}", "\u{d55c}"),
    ("cafe\u{301}.txt", "caf\u{e9}.txt"),
];

/// "As asked" for each rename's new name, where the disk holds only the
/// human's: the sandbox shows both names as two files.
fn new_names_as_asked(path: &Path) -> Option<Spelled> {
    let new = RENAMES
        .iter()
        .any(|(_, to)| path == Path::new("/").join(to));
    new.then_some(Spelled::AsAsked)
}

/// The spelling read the new name as a file of its own, as the disk read
/// it, so the rename listed only the human's name as deleted. Applying
/// deleted it, the one file there was, and the sandbox still showed the
/// new name, its only copy in memory. An empty file too: on exFAT and FAT
/// it has no inode number of its own on the disk, only one per `lstat`
/// the system makes up, the same under both spellings.
#[tokio::test]
async fn a_rename_the_spelling_reads_as_a_file_of_its_own_deletes_nothing() {
    for text in [HUMAN, ""] {
        for (from, to) in RENAMES {
            let (_t, dir) = project();
            fs::write(dir.join(from), text).unwrap();
            if !dir.join(to).exists() {
                // The disk tells the two names apart: nothing to lose.
                continue;
            }
            let ws = SandboxWorkspace::open_told(&dir, new_names_as_asked).await;
            run(&ws, &dir, &format!("mv '{from}' '{to}'")).await;
            let report = ws.changes().unwrap().apply().await;
            assert_eq!(texts(&dir), [text], "{from} -> {to}: {report:?}");
            let shown = ws.fs().read(&dir.join(to)).await.unwrap();
            assert_eq!(shown, text.as_bytes(), "{from} -> {to}");
        }
    }
}

/// The sandbox shows `readme.md` beside the human's `README.md`, one file
/// on the disk: removing it removed the human's file, which the sandbox
/// still listed.
#[tokio::test]
async fn a_delete_of_a_name_the_disk_holds_under_another_never_takes_that_one() {
    let (_t, dir) = project();
    fs::write(dir.join("README.md"), HUMAN).unwrap();
    if !dir.join("readme.md").exists() {
        return;
    }
    let ws = SandboxWorkspace::open_told(&dir, new_names_as_asked).await;
    run(&ws, &dir, "rm readme.md").await;
    let report = ws.changes().unwrap().apply().await;
    assert!(report.applied.is_empty(), "{report:?}");
    assert_eq!(held(&dir), [("README.md".into(), HUMAN.into())]);
}

/// The model copies `README.md` to `readme.md`, one file on the disk, and
/// rewrites `README.md`: the rewrite went over the copy's text, the human's,
/// which then was only in the sandbox.
#[tokio::test]
async fn an_overwrite_never_takes_a_file_the_sandbox_shows_under_another_name() {
    let (_t, dir) = project();
    fs::write(dir.join("README.md"), HUMAN).unwrap();
    if !dir.join("readme.md").exists() {
        return;
    }
    let ws = SandboxWorkspace::open_told(&dir, new_names_as_asked).await;
    run(&ws, &dir, "cp README.md readme.md && echo new > README.md").await;
    let report = ws.changes().unwrap().apply().await;
    assert!(report.applied.is_empty(), "{report:?}");
    assert_eq!(texts(&dir), [HUMAN], "{report:?}");
}

/// While set, the spelling reads `notes.md`, which the human made, as not
/// there.
static HIDING: AtomicBool = AtomicBool::new(false);

fn hides_notes(path: &Path) -> Option<Spelled> {
    let hidden = HIDING.load(Ordering::Relaxed) && path == Path::new("/notes.md");
    hidden.then_some(Spelled::Other(None))
}

/// The sandbox showed no `notes.md` (its listing kept from before the human
/// made it), so the model wrote one; the ledger read the disk apart and took
/// the human's file for the one the model saw, and applying wrote over it.
#[tokio::test]
async fn a_file_the_sandbox_showed_as_absent_is_never_written_over() {
    let (_t, dir) = project();
    fs::write(dir.join("notes.md"), HUMAN).unwrap();
    HIDING.store(true, Ordering::Relaxed);
    let ws = SandboxWorkspace::open_told(&dir, hides_notes).await;
    let notes = dir.join("notes.md");
    assert!(!ws.fs().exists(&notes).await, "the sandbox shows notes.md");
    ws.fs().write(&notes, b"the model's\n").await.unwrap();
    // The answer is right again by the apply.
    HIDING.store(false, Ordering::Relaxed);
    let report = ws.changes().unwrap().apply().await;
    assert!(report.applied.is_empty(), "{report:?}");
    assert_eq!(held(&dir), [("notes.md".into(), HUMAN.into())]);
    let left = ws.changes().unwrap().list().await;
    assert!(left.iter().all(|c| c.conflict), "{left:?}");
}
