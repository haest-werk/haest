//! Edit and Write in the real workspace, on a disk with no room left, and on
//! one that reads two spellings of a name as one. Opt-in: it needs a small
//! disk it may fill (a disk image, a tmpfs).

use std::{
    fmt::Write as _,
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
use tokio::sync::Mutex;

/// Names a directory on a small disk this test may fill to its last byte (a
/// disk image, a tmpfs of a few MiB); unset, the test passes without
/// running.
const SMALL_DISK_DIR: &str = "HAEST_TEST_SMALL_DISK_DIR";

/// Held by each test on the small disk: one fills it, and the others would
/// find no room.
static DISK: Mutex<()> = Mutex::const_new(());

fn input(v: &Value) -> JsonMap {
    v.as_object().cloned().expect("a JSON object")
}

/// The names in `dir`, without the `._<name>` file macOS keeps beside a
/// listed `<name>` on a disk with no extended attributes (exFAT, FAT): the
/// system's own, which goes wherever its file goes.
fn names(dir: &Path) -> Vec<String> {
    let all: Vec<String> = fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    let companion = |n: &str| {
        n.strip_prefix("._")
            .is_some_and(|of| all.iter().any(|a| a == of))
    };
    let mut own: Vec<String> = all.iter().filter(|n| !companion(n)).cloned().collect();
    own.sort();
    own
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
    let _disk = DISK.lock().await;
    let t = tempfile::tempdir_in(base).unwrap();
    let dir = fs::canonicalize(t.path()).unwrap();
    let file = dir.join("src.rs");
    let human = (1..=8000).fold(String::new(), |mut all, i| {
        writeln!(all, "fn line_{i}() {{}}").unwrap();
        all
    });
    fs::write(&file, &human).unwrap();
    let filler = fill(&dir);
    let ctx = ToolContext::new(&dir, RealWorkspace::new(ChildEnv::default()));

    let longer = format!("fn line_4000() {{}} // {}", "x".repeat(16 << 10));
    let edit = json!({"file_path": file, "old_string": "fn line_4000() {}", "new_string": longer});
    let edited = EditTool::new().execute(input(&edit), &ctx).await;
    assert!(edited.is_err(), "the full disk took the edit: {edited:?}");
    let on_disk = fs::read_to_string(&file).unwrap();
    assert!(on_disk == human, "after the edit: {} bytes", on_disk.len());

    let overwrite = json!({"file_path": file, "content": human.repeat(2)});
    let result = WriteTool::new().execute(input(&overwrite), &ctx).await;
    assert!(result.is_err(), "the full disk took the write: {result:?}");
    let on_disk = fs::read_to_string(&file).unwrap();
    assert!(on_disk == human, "after the write: {} bytes", on_disk.len());
    assert_eq!(names(&dir), ["fill", "src.rs"], "nothing left beside it");

    // Room is made: the edit goes in.
    fs::remove_file(filler).unwrap();
    EditTool::new().execute(input(&edit), &ctx).await.unwrap();
    let on_disk = fs::read_to_string(&file).unwrap();
    assert!(on_disk.contains(&longer), "the edit is not in");
    assert!(on_disk.starts_with("fn line_1() {}\n"));
    assert!(on_disk.ends_with("fn line_8000() {}\n"));
    assert_eq!(names(&dir), ["src.rs"]);
}

/// On a disk that reads two spellings of a name as one and keeps the
/// spelling a rename gives (HFS+, exFAT and FAT on macOS; a disk image
/// `hdiutil` makes is HFS+ and reads `readme.md` as `README.md`), an Edit
/// of `readme.md` renamed the human's `README.md` to `readme.md`, and so
/// did a Write: the new file was renamed over the old one under the
/// model's spelling. The file now keeps the name its directory lists, an
/// empty one too (on exFAT and FAT it has no inode number of its own). On
/// a disk that keeps the name it holds through a rename (APFS, vfat on
/// Linux), or tells the spellings apart (ext4, tmpfs), it passes either
/// way.
#[tokio::test]
async fn an_edit_or_a_write_spelled_another_way_keeps_the_files_name() {
    let Some(base) = std::env::var_os(SMALL_DISK_DIR) else {
        return;
    };
    let _disk = DISK.lock().await;
    let t = tempfile::tempdir_in(base).unwrap();
    let dir = fs::canonicalize(t.path()).unwrap();
    fs::write(dir.join("README.md"), "hello world\n").unwrap();
    fs::write(dir.join("TODO.md"), "").unwrap();
    let asked = dir.join("readme.md");
    if !asked.exists() {
        return;
    }
    let ctx = ToolContext::new(&dir, RealWorkspace::new(ChildEnv::default()));
    let held = ["README.md", "TODO.md"];

    let edit = json!({"file_path": asked, "old_string": "hello", "new_string": "HELLO"});
    EditTool::new().execute(input(&edit), &ctx).await.unwrap();
    assert_eq!(names(&dir), held, "after the edit");
    let on_disk = fs::read_to_string(dir.join("README.md")).unwrap();
    assert_eq!(on_disk, "HELLO world\n");

    let overwrite = json!({"file_path": asked, "content": "rewritten\n"});
    WriteTool::new()
        .execute(input(&overwrite), &ctx)
        .await
        .unwrap();
    assert_eq!(names(&dir), held, "after the write");
    let on_disk = fs::read_to_string(dir.join("README.md")).unwrap();
    assert_eq!(on_disk, "rewritten\n");

    let fill_in = json!({"file_path": dir.join("todo.md"), "content": "a list\n"});
    WriteTool::new()
        .execute(input(&fill_in), &ctx)
        .await
        .unwrap();
    assert_eq!(names(&dir), held, "after the write of the empty one");
    let on_disk = fs::read_to_string(dir.join("TODO.md")).unwrap();
    assert_eq!(on_disk, "a list\n");
}
