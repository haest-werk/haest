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

/// The most a disk this test fills may hold: 1 GiB.
const SMALL: u64 = 1 << 30;

/// Held by each test on the small disk: one fills it, and the others would
/// find no room.
static DISK: Mutex<()> = Mutex::const_new(());

/// The directory [`SMALL_DISK_DIR`] names; None where it is unset. Panics,
/// before anything is written, where its disk holds more than [`SMALL`] or
/// its size cannot be read: a run pointed at the host's own disk by mistake
/// wrote 78 GB of fill before it was stopped.
fn small_disk() -> Option<PathBuf> {
    let dir = PathBuf::from(std::env::var_os(SMALL_DISK_DIR)?);
    if let Err(refused) = small(&dir) {
        panic!("{refused}");
    }
    Some(dir)
}

/// Whether the disk `dir` is on holds at most [`SMALL`]; Err says why not.
fn small(dir: &Path) -> Result<(), String> {
    let refused = |why: String| {
        Err(format!(
            "{SMALL_DISK_DIR}={}: {why}; this test fills its disk, so it runs only on one of at most 1 GiB (a disk image, a small tmpfs)",
            dir.display()
        ))
    };
    match disk_size(dir) {
        Ok(size) if size <= SMALL => Ok(()),
        Ok(size) => refused(format!("its disk holds {size} bytes")),
        Err(e) => refused(format!("the size of its disk cannot be read ({e})")),
    }
}

/// How many bytes the disk `dir` is on holds, used or not, by statfs(2):
/// macOS's statvfs(3) gives the block count in 32 bits, so a disk of 16 TiB
/// and 500 MiB read as one of 500 MiB.
#[cfg(target_os = "macos")]
#[expect(unsafe_code, reason = "statfs(2) has no std wrapper")]
fn disk_size(dir: &Path) -> std::io::Result<u64> {
    use std::os::unix::ffi::OsStrExt;
    let c = std::ffi::CString::new(dir.as_os_str().as_bytes())?;
    let mut fs = std::mem::MaybeUninit::<libc::statfs>::uninit();
    // SAFETY: `c` is a NUL-terminated path that outlives the call, and
    // statfs writes one `struct statfs` at the pointer, which has room for
    // it.
    if unsafe { libc::statfs(c.as_ptr(), fs.as_mut_ptr()) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: statfs succeeded, so it filled the struct in.
    let fs = unsafe { fs.assume_init() };
    Ok(fs.f_blocks.saturating_mul(u64::from(fs.f_bsize)))
}

/// How many bytes the disk `dir` is on holds, used or not, by statvfs(3).
#[cfg(not(target_os = "macos"))]
#[expect(unsafe_code, reason = "statvfs(3) has no std wrapper")]
fn disk_size(dir: &Path) -> std::io::Result<u64> {
    use std::os::unix::ffi::OsStrExt;
    let c = std::ffi::CString::new(dir.as_os_str().as_bytes())?;
    let mut vfs = std::mem::MaybeUninit::<libc::statvfs>::uninit();
    // SAFETY: `c` is a NUL-terminated path that outlives the call, and
    // statvfs writes one `struct statvfs` at the pointer, which has room for
    // it.
    if unsafe { libc::statvfs(c.as_ptr(), vfs.as_mut_ptr()) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: statvfs succeeded, so it filled the struct in.
    let vfs = unsafe { vfs.assume_init() };
    Ok(vfs.f_blocks.saturating_mul(vfs.f_frsize))
}

/// Pointed at a disk of more than 1 GiB (the host's own, by mistake), the
/// tests refuse to run, and say why; on a smaller one they run. A path
/// whose disk cannot be told is refused.
#[test]
fn a_disk_of_more_than_1_gib_is_refused() {
    let t = tempfile::tempdir().unwrap();
    let size = disk_size(t.path()).unwrap();
    let got = small(t.path());
    if size > SMALL {
        let refused = got.expect_err("a disk of more than 1 GiB taken");
        assert!(
            refused.contains(&format!("holds {size} bytes")),
            "{refused}"
        );
        assert!(refused.contains("at most 1 GiB"), "{refused}");
    } else {
        assert_eq!(got, Ok(()), "a disk of {size} bytes");
    }
    assert!(small(&t.path().join("not there")).is_err());
}

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
/// ever smaller pieces, each written to the disk before the next. Panics
/// once it took more than [`SMALL`], where the disk said it held less.
fn fill(dir: &Path) -> PathBuf {
    let path = dir.join("fill");
    let mut file = fs::File::create(&path).unwrap();
    let mut written: u64 = 0;
    for piece in [1 << 20, 1 << 16, 1 << 12, 1 << 9, 1_u32] {
        let bytes = vec![0u8; piece as usize];
        while file
            .write_all(&bytes)
            .and_then(|()| file.sync_all())
            .is_ok()
        {
            written += u64::from(piece);
            assert!(written <= SMALL, "the disk took more than 1 GiB of fill");
        }
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
    let Some(base) = small_disk() else {
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

/// On a disk that reads two spellings of a name as one, an Edit or a Write
/// of `readme.md` goes to the human's `README.md`, an empty `TODO.md`
/// too (on exFAT and FAT it has no inode number of its own). The system
/// decides which name the file goes by afterwards, as for any program that
/// saves by renaming (HFS+, exFAT and FAT on macOS take the model's, APFS
/// and vfat on Linux keep the one held): one file, the new text, nothing
/// beside it. On a disk that tells the spellings apart it passes without
/// running.
#[tokio::test]
async fn an_edit_or_a_write_spelled_another_way_keeps_one_file() {
    let Some(base) = small_disk() else {
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
    // The one file under either name, and what it holds.
    let held = |upper: &str, lower: &str| {
        let names = names(&dir);
        let one = names.iter().filter(|n| *n == upper || *n == lower).count();
        assert_eq!(one, 1, "{upper}: {names:?}");
        assert_eq!(names.len(), 2, "{names:?}");
        fs::read_to_string(dir.join(upper)).unwrap()
    };

    let edit = json!({"file_path": asked, "old_string": "hello", "new_string": "HELLO"});
    EditTool::new().execute(input(&edit), &ctx).await.unwrap();
    assert_eq!(held("README.md", "readme.md"), "HELLO world\n");

    let overwrite = json!({"file_path": asked, "content": "rewritten\n"});
    WriteTool::new()
        .execute(input(&overwrite), &ctx)
        .await
        .unwrap();
    assert_eq!(held("README.md", "readme.md"), "rewritten\n");

    let fill_in = json!({"file_path": dir.join("todo.md"), "content": "a list\n"});
    WriteTool::new()
        .execute(input(&fill_in), &ctx)
        .await
        .unwrap();
    assert_eq!(held("TODO.md", "todo.md"), "a list\n");
}
