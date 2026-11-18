//! Edit and Write in the real workspace, on a disk with no room left. Opt-in:
//! it needs a small disk it may fill (a disk image, a tmpfs).

use std::{
    fs,
    io::Write as _,
    path::{Path, PathBuf},
};

use haest_core::{
    ChildEnv,
    tool::{JsonMap, Tool, ToolContext},
};
use haest_tools::{EditTool, WriteTool};
use haest_workspace::RealWorkspace;
use serde_json::{Value, json};

/// Names a directory on a small disk this test may fill to its last byte (a
/// disk image, a tmpfs of a few MiB); unset, the test passes without
/// running.
const SMALL_DISK_DIR: &str = "HAEST_TEST_SMALL_DISK_DIR";

fn input(v: Value) -> JsonMap {
    v.as_object().cloned().expect("a JSON object")
}

fn names(dir: &Path) -> Vec<String> {
    let mut all: Vec<String> = fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    all.sort();
    all
}

/// Fill the disk `dir` is on with `dir/fill` until it takes no more: in
/// ever smaller pieces, each written to the disk before the next.
fn fill(dir: &Path) -> PathBuf {
    let path = dir.join("fill");
    let mut file = fs::File::create(&path).unwrap();
    for piece in [1 << 20, 1 << 16, 1 << 12, 1 << 9, 1] {
        let bytes = vec![0u8; piece];
        while file
            .write_all(&bytes)
            .and_then(|()| file.sync_all())
            .is_ok()
        {}
    }
    path
}

/// An Edit the disk had no room for wrote the edited text with a write
/// that empties the file first: the human's file was left empty (APFS,
/// HFS+) or with its tail cut off (ext4, tmpfs), and the model, holding only
/// what it replaced, could not put it back. Write over it did the same.
/// Both now fail with the file as it was, and nothing beside it.
#[tokio::test]
async fn an_edit_the_full_disk_stops_leaves_the_humans_file_whole() {
    let Some(base) = std::env::var_os(SMALL_DISK_DIR) else {
        return;
    };
    let t = tempfile::tempdir_in(base).unwrap();
    let dir = fs::canonicalize(t.path()).unwrap();
    let file = dir.join("src.rs");
    let human: String = (1..=8000)
        .map(|i| format!("fn line_{i}() {{}}\n"))
        .collect();
    fs::write(&file, &human).unwrap();
    let filler = fill(&dir);
    let ctx = ToolContext::new(&dir, RealWorkspace::new(ChildEnv::default()));

    let longer = format!("fn line_4000() {{}} // {}", "x".repeat(16 << 10));
    let edit = json!({"file_path": file, "old_string": "fn line_4000() {}", "new_string": longer});
    let edited = EditTool::new().execute(input(edit.clone()), &ctx).await;
    assert!(edited.is_err(), "the full disk took the edit: {edited:?}");
    let on_disk = fs::read_to_string(&file).unwrap();
    assert!(on_disk == human, "after the edit: {} bytes", on_disk.len());

    let write = json!({"file_path": file, "content": human.repeat(2)});
    let wrote = WriteTool::new().execute(input(write), &ctx).await;
    assert!(wrote.is_err(), "the full disk took the write: {wrote:?}");
    let on_disk = fs::read_to_string(&file).unwrap();
    assert!(on_disk == human, "after the write: {} bytes", on_disk.len());
    assert_eq!(names(&dir), ["fill", "src.rs"], "nothing left beside it");

    // Room is made: the edit goes in.
    fs::remove_file(filler).unwrap();
    EditTool::new().execute(input(edit), &ctx).await.unwrap();
    let on_disk = fs::read_to_string(&file).unwrap();
    assert!(on_disk.contains(&longer), "the edit is not in");
    assert!(on_disk.starts_with("fn line_1() {}\n"));
    assert!(on_disk.ends_with("fn line_8000() {}\n"));
    assert_eq!(names(&dir), ["src.rs"]);
}
