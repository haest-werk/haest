//! Paths made absolute and resolved lexically, without touching any file
//! system.

use std::path::{Component, Path, PathBuf};

/// `/a/./b/../c` -> `/a/c`, without touching any file system.
///
/// The result is always absolute: a relative `path` is read from `/`, and a
/// `..` at the root stays at the root (`/../x` -> `/x`). Links are not
/// followed, so `..` after a link goes up from the link, not its target.
///
/// # Examples
///
/// ```
/// use std::path::{Path, PathBuf};
///
/// use haest_core::fs::normalize;
///
/// assert_eq!(normalize(Path::new("/a/./b/../c")), PathBuf::from("/a/c"));
/// assert_eq!(normalize(Path::new("/../x")), PathBuf::from("/x"));
/// assert_eq!(normalize(Path::new("a/b")), PathBuf::from("/a/b"));
/// ```
pub fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::from("/");
    for c in path.components() {
        match c {
            Component::RootDir | Component::Prefix(_) | Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            Component::Normal(s) => out.push(s),
        }
    }
    out
}

/// The absolute path a tool's `p` names: an absolute `p` as it is, a relative
/// one taken from `cwd`. Dots are resolved lexically ([`normalize`]).
///
/// # Examples
///
/// ```
/// use std::path::{Path, PathBuf};
///
/// use haest_core::fs::resolve_path;
///
/// let cwd = Path::new("/project");
/// assert_eq!(resolve_path(cwd, "src/../README.md"), PathBuf::from("/project/README.md"));
/// assert_eq!(resolve_path(cwd, "/etc/hosts"), PathBuf::from("/etc/hosts"));
/// ```
pub fn resolve_path(cwd: &Path, p: &str) -> PathBuf {
    // `join` keeps an absolute `p` as it is.
    normalize(&cwd.join(p))
}
