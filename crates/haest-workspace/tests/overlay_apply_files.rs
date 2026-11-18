//! What applying keeps of a file it writes over, where a new file put in
//! its place would lose it: its other links, its group, the read-only mode
//! the human gave it. And a file in a directory that takes no new file is
//! still written. (Its mode: `an_edit_keeps_the_files_mode` in
//! `overlay_modes.rs`.)

mod common;

use std::{
    fs,
    os::unix::fs::{MetadataExt, PermissionsExt},
    path::Path,
};

use common::{
    sandbox,
    spelling::{apply_all, changes, paths, run, writable_again},
    tempdir,
};
use haest_core::workspace::ApplyFailureCode;

fn set_mode(path: &Path, mode: u32) {
    fs::set_permissions(path, fs::Permissions::from_mode(mode)).unwrap();
}

fn names(dir: &Path) -> Vec<String> {
    let mut all: Vec<String> = fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    all.sort();
    all
}

fn read(path: &Path) -> String {
    fs::read_to_string(path).unwrap()
}

/// `b.txt` is another name for `a.txt`: the sandbox's edit of `a.txt`
/// reads under both once applied, and they stay one file.
#[tokio::test]
async fn a_file_with_another_link_is_written_under_both_names() {
    let (_t, dir) = tempdir();
    fs::write(dir.join("a.txt"), "old\n").unwrap();
    fs::hard_link(dir.join("a.txt"), dir.join("b.txt")).unwrap();
    let ws = sandbox(&dir).await;
    run(&ws, &dir, "echo new > a.txt").await;
    apply_all(&ws).await;
    assert_eq!(read(&dir.join("a.txt")), "new\n");
    assert_eq!(read(&dir.join("b.txt")), "new\n");
    let inode = |name: &str| fs::metadata(dir.join(name)).unwrap().ino();
    assert_eq!(inode("a.txt"), inode("b.txt"));
}

/// The human makes `a.txt` read-only once the sandbox has edited it: the
/// apply fails it, and leaves it, and its directory, as they were. Its
/// directory is writable, so a file put in its place would go in.
#[tokio::test]
async fn a_read_only_file_fails_and_is_left_as_it_was() {
    let (_t, dir) = tempdir();
    fs::write(dir.join("a.txt"), "old\n").unwrap();
    let ws = sandbox(&dir).await;
    run(&ws, &dir, "echo new > a.txt").await;
    set_mode(&dir.join("a.txt"), 0o444);
    let report = changes(&ws).apply().await;
    let failed: Vec<_> = report
        .failed
        .iter()
        .map(|f| (f.change.path.as_str(), f.code))
        .collect();
    assert_eq!(failed, [("/a.txt", ApplyFailureCode::WriteFailed)]);
    assert_eq!(read(&dir.join("a.txt")), "old\n");
    let mode = fs::metadata(dir.join("a.txt"))
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(mode & 0o7777, 0o444);
    assert_eq!(names(&dir), ["a.txt"]);
    assert_eq!(paths(&changes(&ws).list().await), ["/a.txt"]);
}

/// The human makes `locked` read-only once the sandbox has edited the
/// file in it: the file may still be written, so it is.
#[tokio::test]
async fn a_writable_file_in_a_read_only_directory_is_still_written() {
    let (_t, dir) = tempdir();
    let locked = dir.join("locked");
    fs::create_dir(&locked).unwrap();
    fs::write(locked.join("a.txt"), "old\n").unwrap();
    let ws = sandbox(&dir).await;
    run(&ws, &dir, "echo new > locked/a.txt").await;
    set_mode(&locked, 0o555);
    let report = changes(&ws).apply().await;
    writable_again(&locked);
    assert!(
        report.failed.is_empty() && report.skipped.is_empty(),
        "{report:?}"
    );
    assert_eq!(read(&locked.join("a.txt")), "new\n");
    assert_eq!(names(&locked), ["a.txt"]);
}

/// The groups this process is in.
fn my_groups() -> Vec<u32> {
    let out = std::process::Command::new("id").arg("-G").output().unwrap();
    assert!(out.status.success(), "id -G: {out:?}");
    String::from_utf8_lossy(&out.stdout)
        .split_whitespace()
        .map(|g| g.parse().unwrap())
        .collect()
}

/// `shared/a.txt` is in one group, `shared` in another, whose group a new
/// file in it takes (setgid): the file keeps its own group once applied.
/// Skipped where this process is in one group only.
#[tokio::test]
async fn a_file_keeps_its_group_where_its_directory_gives_another() {
    let (_t, dir) = tempdir();
    let shared = dir.join("shared");
    fs::create_dir(&shared).unwrap();
    fs::write(shared.join("a.txt"), "old\n").unwrap();
    let own = fs::metadata(shared.join("a.txt")).unwrap().gid();
    let Some(other) = my_groups().into_iter().find(|&g| g != own) else {
        return;
    };
    std::os::unix::fs::chown(&shared, None, Some(other)).unwrap();
    set_mode(&shared, 0o2775);
    let ws = sandbox(&dir).await;
    run(&ws, &dir, "echo new > shared/a.txt").await;
    apply_all(&ws).await;
    assert_eq!(read(&shared.join("a.txt")), "new\n");
    assert_eq!(fs::metadata(shared.join("a.txt")).unwrap().gid(), own);
}

/// Names a directory that passes its group on (setgid) to a group this
/// process is not in (only root can make one), for the test of it; unset,
/// the test passes without running.
#[cfg(target_os = "linux")]
const FOREIGN_SETGID_DIR: &str = "HAEST_TEST_FOREIGN_SETGID_DIR";

/// A team's directory passes its group on (setgid) to a group the run is
/// not in: a user added to the group who has not logged in again, a
/// container run as the human's uid without the team's group. Linux drops
/// the bit where such a run sets the mode of a directory it made there,
/// and the apply's new file, made in a directory of its own, got the run's
/// group: the edit of `edit.txt` was written in place (cut short on a full
/// disk), and `new.txt` was added in the run's group. Both now take the
/// directory's group, and the edit replaces the file.
#[cfg(target_os = "linux")]
#[tokio::test]
async fn a_directory_whose_group_the_run_is_not_in_still_gives_it_on() {
    let Some(base) = std::env::var_os(FOREIGN_SETGID_DIR) else {
        return;
    };
    let t = tempfile::tempdir_in(base).unwrap();
    let dir = fs::canonicalize(t.path()).unwrap();
    let group = fs::metadata(&dir).unwrap().gid();
    let passes_on = fs::metadata(&dir).unwrap().mode() & 0o2000 != 0;
    assert!(passes_on, "{} passes no group on", dir.display());
    let member = my_groups().contains(&group);
    assert!(!member, "this run is in group {group}");
    fs::write(dir.join("edit.txt"), "old\n").unwrap();
    let before = fs::metadata(dir.join("edit.txt")).unwrap().ino();
    let ws = sandbox(&dir).await;
    run(&ws, &dir, "echo new > edit.txt; echo added > new.txt").await;
    apply_all(&ws).await;
    let meta = |name: &str| fs::metadata(dir.join(name)).unwrap();
    assert_eq!(read(&dir.join("edit.txt")), "new\n");
    assert_ne!(meta("edit.txt").ino(), before, "written in place");
    assert_eq!(meta("edit.txt").gid(), group, "edit.txt's group");
    assert_eq!(meta("new.txt").gid(), group, "new.txt's group");
}
