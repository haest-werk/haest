//! The real disk's whole write ([`Fs::write_whole`]), which the tools that
//! rewrite a file (Edit, Write over one) and the apply go through.

use std::{
    fs::{self, Permissions},
    os::unix::fs::{MetadataExt, PermissionsExt, symlink},
    path::Path,
    time::Duration,
};

use haest_core::fs::Fs;

use crate::{
    RealFs,
    real::testing::{mkfifo, release},
};

fn names(dir: &Path) -> Vec<String> {
    let mut all: Vec<String> = fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    all.sort();
    all
}

/// The real disk replaces a file that is there with a new one, renamed
/// over it with its permission bits, and makes one that is not, as open as
/// a plain write makes it.
#[tokio::test]
async fn the_real_disk_replaces_a_file_whole_and_keeps_its_mode() {
    let t = tempfile::tempdir().unwrap();
    let real = fs::canonicalize(t.path()).unwrap().join("notes.txt");
    fs::write(&real, "the human's notes\n").unwrap();
    fs::set_permissions(&real, Permissions::from_mode(0o640)).unwrap();
    let before = fs::metadata(&real).unwrap();

    RealFs.write_whole(&real, b"edited\n").await.unwrap();
    assert_eq!(fs::read_to_string(&real).unwrap(), "edited\n");
    let after = fs::metadata(&real).unwrap();
    assert_ne!(after.ino(), before.ino(), "written in place, not replaced");
    assert_eq!(after.permissions().mode() & 0o7777, 0o640);
    assert_eq!(names(t.path()), ["notes.txt"]);

    let new = real.with_file_name("new.txt");
    RealFs.write_whole(&new, b"made\n").await.unwrap();
    assert_eq!(fs::read_to_string(&new).unwrap(), "made\n");
    assert_eq!(names(t.path()), ["new.txt", "notes.txt"]);
    let plain = real.with_file_name("plain.txt");
    fs::write(&plain, "").unwrap();
    let mode = |p: &Path| fs::metadata(p).unwrap().permissions().mode() & 0o7777;
    assert_eq!(mode(&new), mode(&plain));
}

/// A Write onto a FIFO a reader reads once (`cat pipe > got`). The whole
/// write opened it to look at it before it saw what it was: the reader got
/// an empty message and left, and the write that followed waited for a
/// reader that never came. It now opens it once, as a plain write does.
#[tokio::test]
async fn a_fifo_is_opened_once_and_its_reader_gets_the_bytes() {
    let t = tempfile::tempdir().unwrap();
    let fifo = t.path().join("pipe");
    mkfifo(&fifo);
    let reader = std::thread::spawn({
        let fifo = fifo.clone();
        move || fs::read(fifo)
    });

    let wait = Duration::from_secs(2);
    let wrote = tokio::time::timeout(wait, RealFs.write_whole(&fifo, b"x")).await;
    // Nothing is left waiting on the pipe, whatever came of it.
    release(&fifo);
    let got = reader.join().unwrap().unwrap();
    assert!(matches!(wrote, Ok(Ok(()))), "the write: {wrote:?}");
    assert_eq!(got, b"x");
}

/// A symlink is written through, in place: the link stays a link, and the
/// file it names, in another directory, takes the bytes under its own name.
#[tokio::test]
async fn a_symlink_is_written_through_in_place() {
    let t = tempfile::tempdir().unwrap();
    let root = fs::canonicalize(t.path()).unwrap();
    let (here, there) = (root.join("here"), root.join("there"));
    fs::create_dir(&here).unwrap();
    fs::create_dir(&there).unwrap();
    let target = there.join("Target.txt");
    fs::write(&target, "the human's notes\n").unwrap();
    let before = fs::metadata(&target).unwrap().ino();
    let link = here.join("link.txt");
    symlink(&target, &link).unwrap();

    RealFs.write_whole(&link, b"edited\n").await.unwrap();
    assert!(fs::symlink_metadata(&link).unwrap().is_symlink());
    assert_eq!(fs::read_link(&link).unwrap(), target);
    assert_eq!(fs::read_to_string(&target).unwrap(), "edited\n");
    assert_eq!(fs::metadata(&target).unwrap().ino(), before);
    assert_eq!(names(&here), ["link.txt"]);
    assert_eq!(names(&there), ["Target.txt"]);
}
