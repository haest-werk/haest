//! For the tests on a disk that reads some spellings of a name as one
//! (macOS's default: case and Unicode normalization; case-sensitive APFS:
//! normalization only): what the disk holds, and the sandbox's changes.

use std::{fs, path::Path};

use haest_core::workspace::{Change, ChangeSetRef, Workspace};
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
/// its content.
pub(crate) fn contents(dir: &Path) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut pending = vec![dir.to_path_buf()];
    while let Some(d) = pending.pop() {
        for e in fs::read_dir(&d).unwrap() {
            let p = e.unwrap().path();
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
