//! What a path looked like: a file (its content hash), a directory, a symlink
//! (its target's hash), or absent. A link is never followed: a link to a file
//! is not that file, and one that dangles or leaves the project is still there.

use std::{
    hash::{DefaultHasher, Hash, Hasher},
    path::{Path, PathBuf},
};

use haest_core::fs::{FileKind, Fs};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Original {
    File(u64),
    Dir,
    Link(u64),
    Absent,
}

fn hash_of(bytes: &[u8]) -> u64 {
    let mut h = DefaultHasher::new();
    bytes.hash(&mut h);
    h.finish()
}

pub(crate) async fn state_of(fs: &dyn Fs, path: &Path) -> Original {
    if let Ok(target) = fs.read_link(path).await {
        return Original::Link(hash_of(target.as_os_str().as_encoded_bytes()));
    }
    if !fs.exists(path).await {
        return Original::Absent;
    }
    if fs.stat(path).await.is_ok_and(|s| s.is_dir()) {
        return Original::Dir;
    }
    match fs.read(path).await {
        Ok(bytes) => Original::File(hash_of(&bytes)),
        // There but unreadable: the reason stands in for the content, so it
        // still differs from any readable version.
        Err(e) => Original::File(hash_of(format!("unreadable: {e}").as_bytes())),
    }
}

/// Every path under `dir` (directories and files). A symlinked directory is
/// listed, never descended, so a link loop cannot spin the walk.
pub(crate) async fn walk(fs: &dyn Fs, dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut pending = vec![dir.to_path_buf()];
    while let Some(d) = pending.pop() {
        for e in fs.read_dir(&d).await.unwrap_or_default() {
            let path = d.join(&e.name);
            if e.kind == FileKind::Dir {
                pending.push(path.clone());
            }
            out.push(path);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use haest_core::fs::MemFs;

    #[tokio::test]
    async fn a_path_is_a_file_hash_a_directory_or_absent() {
        let fs = MemFs::from_files([("/d/a.txt", "A"), ("/d/b.txt", "A"), ("/d/c.txt", "C")]);
        let a = state_of(&fs, Path::new("/d/a.txt")).await;
        assert_eq!(a, state_of(&fs, Path::new("/d/b.txt")).await);
        assert_ne!(a, state_of(&fs, Path::new("/d/c.txt")).await);
        assert_eq!(state_of(&fs, Path::new("/d")).await, Original::Dir);
        assert_eq!(state_of(&fs, Path::new("/nope")).await, Original::Absent);
    }

    #[tokio::test]
    async fn walk_lists_every_path_below_a_directory() {
        let fs = MemFs::from_files([("/d/a", "1"), ("/d/s/b", "2")]);
        let mut all = walk(&fs, Path::new("/d")).await;
        all.sort();
        assert_eq!(all, ["/d/a", "/d/s", "/d/s/b"].map(PathBuf::from).to_vec());
    }
}
