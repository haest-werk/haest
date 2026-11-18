//! The real disk's whole write ([`Fs::write_whole`]), which the tools that
//! rewrite a file (Edit, Write over one) go through: the apply's own.

use std::{
    fs::{self, Permissions},
    os::unix::fs::{MetadataExt, PermissionsExt},
    sync::atomic::Ordering,
};

use haest_core::fs::Fs;

use super::{faulty::Faulty, names};
use crate::RealFs;

/// The disk fills up while a tool rewrites `notes.txt`. The write emptied
/// the file first, so the disk stopped it with the human's file cut short:
/// emptied on a disk that frees the old blocks only later (APFS), its tail
/// gone on one that frees them at once (ext4, tmpfs). The model holds only
/// what it replaced and could not put it back. The file now stays as it
/// was, and nothing is left beside it.
#[tokio::test]
async fn a_whole_write_the_full_disk_stops_leaves_the_file_as_it_was() {
    let t = tempfile::tempdir().unwrap();
    let real = fs::canonicalize(t.path()).unwrap().join("notes.txt");
    let human = "the human's notes\n".repeat(50);
    fs::write(&real, &human).unwrap();
    let disk = Faulty::new(&real, None);
    disk.room.store(64, Ordering::SeqCst);

    let edited = "the edited notes, longer\n".repeat(50);
    let wrote = disk.write_whole(&real, edited.as_bytes()).await;
    assert!(wrote.is_err(), "the full disk took it all");
    let on_disk = fs::read_to_string(&real).unwrap();
    assert!(
        on_disk == human,
        "the human's file: {} bytes",
        on_disk.len()
    );
    assert_eq!(names(t.path()), ["notes.txt"], "nothing left beside it");

    // Room is made: the next one writes it.
    disk.room.store(usize::MAX, Ordering::SeqCst);
    disk.write_whole(&real, edited.as_bytes()).await.unwrap();
    assert_eq!(fs::read_to_string(&real).unwrap(), edited);
    assert_eq!(names(t.path()), ["notes.txt"]);
}

/// The real disk replaces a file that is there with a new one, renamed
/// over it with its permission bits, and makes one that is not.
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
}
