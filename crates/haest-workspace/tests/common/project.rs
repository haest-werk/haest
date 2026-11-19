//! What the disk holds under a project, and the sandbox's changes to it;
//! on a disk that reads two names of a file as one (macOS's default: case
//! and Unicode normalization; case-sensitive APFS: normalization only;
//! exFAT, FAT, vfat) or not.

use std::{ffi::OsString, fs, path::Path};

use haest_core::workspace::{ApplyReport, Change, ChangeSetRef, Workspace};
use haest_workspace::SandboxWorkspace;

use super::opts;

/// Run `command` in the sandbox; it must succeed.
pub(crate) async fn run(ws: &impl Workspace, dir: &Path, command: &str) {
    let r = ws.shell(command, opts(dir)).await;
    assert_eq!(r.exit_code, 0, "{command}: {r:?}");
}

pub(crate) fn changes(ws: &SandboxWorkspace) -> ChangeSetRef {
    ws.changes().expect("the sandbox records its changes")
}

/// Whether this disk reads `other` as the name `name` already in `dir`.
pub(crate) fn reads_as_one(dir: &Path, name: &str, other: &str) -> bool {
    assert!(dir.join(name).exists(), "{name} is in {}", dir.display());
    dir.join(other).exists()
}

/// Whether the disk under `dir` reads names without case, asked with a file
/// that is gone again once it answers.
pub(crate) fn folds_case(dir: &Path) -> bool {
    fs::write(dir.join("probe"), "").unwrap();
    let one = reads_as_one(dir, "probe", "PROBE");
    fs::remove_file(dir.join("probe")).unwrap();
    one
}

/// Every file under `dir`, by its path inside it as the disk lists it, with
/// its content. Not the `._<name>` file macOS keeps beside a listed
/// `<name>` on a disk with no extended attributes (exFAT, FAT): the
/// system's own, which goes wherever its file goes.
pub(crate) fn contents(dir: &Path) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut pending = vec![dir.to_path_buf()];
    while let Some(d) = pending.pop() {
        let names: Vec<OsString> = fs::read_dir(&d)
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        let companion = |n: &OsString| {
            n.as_encoded_bytes()
                .strip_prefix(b"._")
                .is_some_and(|of| names.iter().any(|a| a.as_encoded_bytes() == of))
        };
        for name in names.iter().filter(|n| !companion(n)) {
            let p = d.join(name);
            if p.is_dir() {
                pending.push(p);
            } else {
                let rel = p.strip_prefix(dir).unwrap().to_string_lossy().into_owned();
                out.push((rel, fs::read_to_string(&p).unwrap()));
            }
        }
    }
    out.sort();
    out
}

/// The changes' paths, sorted: apply reports the deletes first.
pub(crate) fn paths(changes: &[Change]) -> Vec<String> {
    let mut all: Vec<_> = changes.iter().map(|c| c.path.clone()).collect();
    all.sort();
    all
}

pub(crate) fn pairs(want: &[(&str, &str)]) -> Vec<(String, String)> {
    want.iter()
        .map(|(p, b)| ((*p).to_owned(), (*b).to_owned()))
        .collect()
}

/// The mode of the directory `dir`, which is then made writable again
/// (0755) so the temp dir around it can go; None when it is not there.
pub(crate) fn writable_again(dir: &Path) -> Option<u32> {
    use std::os::unix::fs::PermissionsExt;
    let was = fs::metadata(dir).ok()?.permissions().mode() & 0o7777;
    let _ = fs::set_permissions(dir, fs::Permissions::from_mode(0o755));
    Some(was)
}

/// Apply everything the sandbox changed; nothing may be skipped or fail, and
/// nothing is left listed.
pub(crate) async fn apply_all(ws: &SandboxWorkspace) {
    let report = changes(ws).apply().await;
    assert!(
        report.failed.is_empty() && report.skipped.is_empty(),
        "{report:?}"
    );
    let left = changes(ws).list().await;
    assert!(left.is_empty(), "left {left:?} after {report:?}");
}

/// After `report`: nothing failed, what was not applied was skipped as a
/// conflict, what is still listed was skipped, and every file the sandbox
/// shows is on the disk as the sandbox shows it, by its name or one the
/// disk reads as it, or still listed.
pub(crate) async fn nothing_lost(ws: &SandboxWorkspace, dir: &Path, report: &ApplyReport) {
    assert!(report.failed.is_empty(), "{report:?}");
    assert!(report.skipped.iter().all(|c| c.conflict), "{report:?}");
    let left = paths(&changes(ws).list().await);
    let skipped = paths(&report.skipped);
    assert!(
        left.iter().all(|p| skipped.contains(p)),
        "left {left:?} after {report:?}"
    );
    let found = ws.shell("find . -type f", opts(dir)).await;
    for file in found.stdout.lines() {
        let rel = file.strip_prefix("./").unwrap_or(file);
        let shown = ws.fs().read(&dir.join(rel)).await.unwrap();
        let on_disk = fs::read(dir.join(rel)).is_ok_and(|held| held == shown);
        assert!(
            on_disk || left.contains(&format!("/{rel}")),
            "{rel}, shown, is neither on the disk nor listed: {report:?}"
        );
    }
}
