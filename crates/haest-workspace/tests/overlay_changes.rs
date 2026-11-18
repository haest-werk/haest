//! Listing and applying the changes over the sandbox's project overlay (port of the TS
//! `overlay-changes.test.ts`, plus the untouched-disk check).

mod common;

use std::{fs, path::Path};

use common::{opts, sandbox, tempdir};
use haest_core::workspace::{
    ApplyFailureCode, ApplyReport, Change, ChangeKind, ChangeSetRef, Workspace,
};
use haest_workspace::{SandboxMounts, SandboxWorkspace};

fn project() -> (tempfile::TempDir, std::path::PathBuf) {
    let (t, dir) = tempdir();
    fs::write(dir.join("keep.txt"), "keep\n").unwrap();
    fs::write(dir.join("edit.txt"), "old line\n").unwrap();
    fs::write(dir.join("gone.txt"), "bye\n").unwrap();
    (t, dir)
}

fn changes(ws: &SandboxWorkspace) -> ChangeSetRef {
    ws.changes().expect("the sandbox records its changes")
}

async fn sandbox_edits(dir: &Path) -> SandboxWorkspace {
    let ws = sandbox(dir).await;
    ws.fs()
        .write(&dir.join("edit.txt"), b"new line\n")
        .await
        .unwrap();
    let r = ws
        .shell(
            "mkdir -p src && echo \"fresh\" > src/new.txt && rm gone.txt",
            opts(dir),
        )
        .await;
    assert_eq!(r.exit_code, 0, "{r:?}");
    ws
}

fn rows(list: &[Change]) -> Vec<(&str, ChangeKind, bool)> {
    list.iter()
        .map(|c| (c.path.as_str(), c.kind, c.conflict))
        .collect()
}

fn read(dir: &Path, rel: &str) -> String {
    fs::read_to_string(dir.join(rel)).unwrap()
}

#[tokio::test]
async fn tool_writes_and_shell_writes_are_both_recorded_and_nothing_reaches_the_disk() {
    let (_t, dir) = project();
    let ws = sandbox_edits(&dir).await;
    let list = changes(&ws).list().await;
    assert_eq!(
        rows(&list),
        [
            ("/edit.txt", ChangeKind::Modified, false),
            ("/gone.txt", ChangeKind::Deleted, false),
            ("/src", ChangeKind::Added, false),
            ("/src/new.txt", ChangeKind::Added, false),
        ]
    );
    assert_eq!(read(&dir, "edit.txt"), "old line\n");
    assert!(dir.join("gone.txt").exists());
    assert!(!dir.join("src").exists());
}

#[tokio::test]
async fn edits_are_recorded_with_a_writable_and_a_readable_mount_beside_the_project() {
    let (_t, dir) = project();
    let (_s, spill) = tempdir();
    let (_k, skills) = tempdir();
    let mounts = SandboxMounts::new(&dir)
        .with_writable(&spill)
        .with_readable(&skills);
    let ws = SandboxWorkspace::open(mounts).await.unwrap();
    ws.fs()
        .write(&dir.join("answer.txt"), b"42\n")
        .await
        .unwrap();
    let list = changes(&ws).list().await;
    assert_eq!(rows(&list), [("/answer.txt", ChangeKind::Added, false)]);
}

#[tokio::test]
async fn apply_puts_every_change_on_disk_after_which_nothing_is_left() {
    let (_t, dir) = project();
    let ws = sandbox_edits(&dir).await;
    let ApplyReport {
        applied,
        skipped,
        failed,
    } = changes(&ws).apply().await;
    assert_eq!(applied.len(), 4);
    assert!(skipped.is_empty() && failed.is_empty());
    assert_eq!(read(&dir, "edit.txt"), "new line\n");
    assert_eq!(read(&dir, "src/new.txt"), "fresh\n");
    assert!(!dir.join("gone.txt").exists());
    assert!(changes(&ws).list().await.is_empty());
}

#[tokio::test]
async fn a_file_the_human_edited_meanwhile_is_a_conflict_skipped_not_overwritten() {
    let (_t, dir) = project();
    let ws = sandbox_edits(&dir).await;
    fs::write(dir.join("edit.txt"), "the human was here\n").unwrap();
    let ApplyReport {
        applied, skipped, ..
    } = changes(&ws).apply().await;
    let skipped: Vec<_> = skipped.iter().map(|c| c.path.as_str()).collect();
    assert_eq!(skipped, ["/edit.txt"]);
    assert!(applied.iter().all(|c| c.path != "/edit.txt"));
    assert_eq!(read(&dir, "edit.txt"), "the human was here\n");
    // Still listed, still a conflict: the human decides.
    let left = changes(&ws).list().await;
    assert_eq!(rows(&left), [("/edit.txt", ChangeKind::Modified, true)]);
}

#[tokio::test]
async fn a_directory_moved_or_copied_whole_is_applied_with_its_files() {
    let (_t, dir) = project();
    fs::create_dir(dir.join("d1")).unwrap();
    fs::write(dir.join("d1/a.txt"), "A\n").unwrap();
    let ws = sandbox(&dir).await;
    let r = ws.shell("mv d1 d2 && cp -r d2 d3", opts(&dir)).await;
    assert_eq!(r.exit_code, 0, "{r:?}");
    let list = changes(&ws).list().await;
    let got: Vec<_> = list.iter().map(|c| (c.path.as_str(), c.kind)).collect();
    assert_eq!(
        got,
        [
            ("/d1", ChangeKind::Deleted),
            ("/d2", ChangeKind::Added),
            ("/d2/a.txt", ChangeKind::Added),
            ("/d3", ChangeKind::Added),
            ("/d3/a.txt", ChangeKind::Added),
        ]
    );
    changes(&ws).apply().await;
    assert!(!dir.join("d1").exists());
    assert_eq!(read(&dir, "d2/a.txt"), "A\n");
    assert_eq!(read(&dir, "d3/a.txt"), "A\n");
}

/// A file the sandbox edited, then removed with its directory: both deletes
/// are listed, and the directory's, first in path order, takes the file
/// with it. The file's delete then finds it gone, as the sandbox has it:
/// applied, not a conflict.
#[tokio::test]
async fn a_file_removed_with_its_directory_is_applied_with_it() {
    let (_t, dir) = project();
    fs::create_dir(dir.join("lib")).unwrap();
    fs::write(dir.join("lib/a.rs"), "a\n").unwrap();
    let ws = sandbox(&dir).await;
    let r = ws
        .shell("echo more >> lib/a.rs && rm -r lib", opts(&dir))
        .await;
    assert_eq!(r.exit_code, 0, "{r:?}");
    let list = changes(&ws).list().await;
    assert_eq!(
        rows(&list),
        [
            ("/lib", ChangeKind::Deleted, false),
            ("/lib/a.rs", ChangeKind::Deleted, false),
        ]
    );
    let report = changes(&ws).apply().await;
    assert_eq!(rows(&report.applied), rows(&list), "{report:?}");
    assert!(
        report.skipped.is_empty() && report.failed.is_empty(),
        "{report:?}"
    );
    assert!(!dir.join("lib").exists());
    assert!(changes(&ws).list().await.is_empty());
}

#[tokio::test]
async fn a_binary_file_is_named_in_the_diff_not_dumped_into_it() {
    let (_t, dir) = project();
    let ws = sandbox(&dir).await;
    ws.fs()
        .write(&dir.join("blob.bin"), &[0, 1, 2, 0, 255])
        .await
        .unwrap();
    let list = changes(&ws).list().await;
    let blob = list.iter().find(|c| c.path == "/blob.bin").unwrap();
    assert_eq!(
        changes(&ws).diff(blob).await,
        "Binary file /blob.bin differs\n"
    );
}

/// B37: a link the sandbox made -- out of the workspace, dangling, to a file,
/// to a directory, or a loop inside a new directory -- is listed as one change
/// and reported as not applied, never an error and never applied as a copy of
/// what it points to.
#[tokio::test]
async fn a_symlink_the_sandbox_made_is_listed_and_reported_never_applied_as_a_copy() {
    let (_t, dir) = project();
    fs::create_dir(dir.join("d")).unwrap();
    fs::write(dir.join("d/a.txt"), "A\n").unwrap();
    let ws = sandbox(&dir).await;
    let r = ws
        .shell(
            "ln -s /etc sub && ln -s missing dangling && ln -s keep.txt l && ln -s d dl \
             && mkdir n && ln -s .. n/up && echo ok > fine.txt",
            opts(&dir),
        )
        .await;
    assert_eq!(r.exit_code, 0, "{r:?}");
    // The shell resolves a link on its way; a tool's write does not.
    ws.fs()
        .write(&dir.join("dl/behind.txt"), b"x\n")
        .await
        .unwrap();
    let set = changes(&ws);
    let list = set.list().await;
    assert!(list.iter().all(|c| c.kind == ChangeKind::Added), "{list:?}");
    let listed: Vec<_> = list.iter().map(|c| c.path.as_str()).collect();
    assert_eq!(
        listed,
        [
            "/dangling",
            "/dl",
            "/dl/behind.txt",
            "/fine.txt",
            "/l",
            "/n",
            "/n/up",
            "/sub"
        ]
    );
    let links = ["/dangling", "/dl", "/dl/behind.txt", "/l", "/n/up", "/sub"];

    let l = set
        .list()
        .await
        .into_iter()
        .find(|c| c.path == "/l")
        .unwrap();
    assert!(set.diff(&l).await.contains("keep.txt"));

    let report = set.apply().await;
    let failed: Vec<_> = report
        .failed
        .iter()
        .map(|f| (f.change.path.as_str(), f.code))
        .collect();
    assert_eq!(failed, links.map(|p| (p, ApplyFailureCode::Symlink)));
    let applied: Vec<_> = report.applied.iter().map(|c| c.path.as_str()).collect();
    assert_eq!(applied, ["/fine.txt", "/n"]);
    assert!(report.skipped.is_empty());
    for link in links {
        let on_disk = dir.join(link.trim_start_matches('/'));
        assert!(fs::symlink_metadata(&on_disk).is_err(), "{link} on disk");
    }
    // Still listed: nothing was lost.
    let left: Vec<_> = set.list().await.into_iter().map(|c| c.path).collect();
    assert_eq!(left, links);
}

#[tokio::test]
async fn the_diff_shows_the_removed_and_the_added_line() {
    let (_t, dir) = project();
    let ws = sandbox_edits(&dir).await;
    let list = changes(&ws).list().await;
    let edit = list.iter().find(|c| c.path == "/edit.txt").unwrap();
    let diff = changes(&ws).diff(edit).await;
    assert!(diff.lines().any(|l| l == "-old line"), "{diff}");
    assert!(diff.lines().any(|l| l == "+new line"), "{diff}");
}
