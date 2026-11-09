use std::path::{Path, PathBuf};

use super::*;

#[tokio::test]
async fn memfs_reads_what_it_wrote_and_lists_direct_children_only() {
    let fs = MemFs::with_files([("/p/a.txt", "A"), ("/p/sub/b.txt", "B")]);
    assert_eq!(fs.read_to_string(Path::new("/p/a.txt")).await.unwrap(), "A");
    let names: Vec<_> = fs
        .read_dir(Path::new("/p"))
        .await
        .unwrap()
        .into_iter()
        .map(|e| e.name)
        .collect();
    assert_eq!(names, ["a.txt", "sub"]);
    assert!(fs.write(Path::new("/nope/x"), b"x").await.is_err());
}

#[tokio::test]
async fn memfs_rename_moves_a_whole_tree_and_remove_needs_recursive() {
    let fs = MemFs::with_files([("/d/x/1", "1")]);
    fs.rename(Path::new("/d/x"), Path::new("/d/y"))
        .await
        .unwrap();
    assert!(fs.exists(Path::new("/d/y/1")).await);
    assert!(!fs.exists(Path::new("/d/x")).await);
    assert!(fs.remove(Path::new("/d"), false).await.is_err());
    fs.remove(Path::new("/d"), true).await.unwrap();
    assert!(!fs.exists(Path::new("/d/y/1")).await);
}

#[test]
fn normalize_resolves_dots_lexically() {
    assert_eq!(normalize(Path::new("/a/./b/../c")), PathBuf::from("/a/c"));
    assert_eq!(normalize(Path::new("/../x")), PathBuf::from("/x"));
}
