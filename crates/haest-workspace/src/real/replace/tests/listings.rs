//! What the apply reads of a file's directory to put the file there.

use std::{fs, path::Path, sync::Arc};

use haest_core::{
    fs::{FsRef, MemFs},
    workspace::ChangeSet,
};

use crate::{
    RealFs, bridge::BashkitFsOverHaest, overlay::OverlayChanges, real::replace::listed::LISTED,
};

/// Each whole write of the apply listed its file's whole directory, to
/// keep the name it lists: an apply of k files in a directory of n read
/// about k·n/2 names, and 10,000 of 10,000 took 30 s where 7 s did. The
/// apply's paths are the disk's own spelling, read as the disk spells
/// them right before each write: it reads that directory once at most.
#[tokio::test]
async fn an_apply_of_many_files_in_one_directory_lists_it_once_at_most() {
    let t = tempfile::tempdir().unwrap();
    let root = fs::canonicalize(t.path()).unwrap();
    let names: Vec<String> = (0..50).map(|i| format!("f{i:02}.txt")).collect();
    for name in &names {
        fs::write(root.join(name), "the human's\n").unwrap();
    }
    let files = MemFs::from_files(
        names
            .iter()
            .map(|n| (Path::new("/").join(n), "the human's\n")),
    );
    let overlay = Arc::new(BashkitFsOverHaest::new(FsRef::new(files)));
    let changes = OverlayChanges::new(overlay, Arc::new(RealFs), &root);
    let recorder = changes.recorder();
    for name in &names {
        let path = Path::new("/").join(name);
        recorder
            .write_file(&path, b"the sandbox's\n")
            .await
            .unwrap();
    }

    let report = changes.apply().await;
    assert_eq!(report.applied.len(), 50, "{report:?}");
    for name in &names {
        assert_eq!(
            fs::read_to_string(root.join(name)).unwrap(),
            "the sandbox's\n"
        );
    }
    let whole_writes = LISTED
        .lock()
        .unwrap()
        .iter()
        .filter(|d| *d == &root)
        .count();
    let lookups = changes.listings_read();
    assert!(
        whole_writes + lookups <= 1,
        "the whole writes listed it {whole_writes} times, the lookups {lookups}"
    );
}
