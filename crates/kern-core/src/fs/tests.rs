use std::{
    io,
    path::{Path, PathBuf},
};

use super::*;

#[tokio::test]
async fn memfs_reads_what_it_wrote_and_lists_direct_children_only() {
    let fs = MemFs::with_files([("/p/a.txt", "A"), ("/p/sub/b.txt", "B")]);
    assert_eq!(fs.read_to_string(Path::new("/p/a.txt")).await.unwrap(), "A");
    let head = |max| fs.read_head(Path::new("/p/a.txt"), max);
    assert_eq!(
        (head(0).await.unwrap(), head(9).await.unwrap()),
        (vec![], b"A".to_vec())
    );
    assert!(fs.read_head(Path::new("/p/sub"), 1).await.is_err());
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

/// What a real fs refuses: a dir moved into itself (EINVAL), the root removed.
#[tokio::test]
async fn memfs_refuses_a_dir_into_itself_and_removing_the_root() {
    let fs = MemFs::with_files([("/a/x", "1")]);
    let err = fs.rename(Path::new("/a"), Path::new("/a/b")).await;
    assert_eq!(err.map_err(|e| e.kind()), Err(io::ErrorKind::InvalidInput));
    assert!(fs.exists(Path::new("/a/x")).await, "nothing moved");
    for recursive in [false, true] {
        assert!(fs.remove(Path::new("/"), recursive).await.is_err());
    }
    fs.write(Path::new("/new"), b"n").await.unwrap();
    assert!(fs.exists(Path::new("/a/x")).await);
}

/// The default `read_link`: a file system without links has none, and a
/// missing path is still missing.
#[tokio::test]
async fn a_file_system_without_links_says_no_path_is_one() {
    use io::ErrorKind::{InvalidInput, NotFound};
    let fs = MemFs::with_files([("/d/a", "1")]);
    for (p, want) in [
        ("/d/a", InvalidInput),
        ("/d", InvalidInput),
        ("/nope", NotFound),
    ] {
        let got = fs.read_link(Path::new(p)).await.unwrap_err().kind();
        assert_eq!(got, want, "{p}");
    }
}

#[test]
fn normalize_resolves_dots_lexically() {
    assert_eq!(normalize(Path::new("/a/./b/../c")), PathBuf::from("/a/c"));
    assert_eq!(normalize(Path::new("/../x")), PathBuf::from("/x"));
}

#[test]
fn relative_paths_resolve_from_cwd() {
    assert_eq!(
        resolve_path(Path::new("/p"), "a/../b.txt"),
        PathBuf::from("/p/b.txt")
    );
    assert_eq!(resolve_path(Path::new("/p"), "/abs"), PathBuf::from("/abs"));
}
