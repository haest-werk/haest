//! The unified diff `/changes` shows for one path.

use std::path::Path;

use kern_core::fs::{Fs, looks_binary};
use similar::TextDiff;

/// Lines of context around each hunk.
const CONTEXT: usize = 2;

/// A file's bytes; empty for a directory or a path that is not there.
pub(crate) async fn file_bytes(fs: &dyn Fs, path: &Path) -> Vec<u8> {
    if !fs.exists(path).await || fs.stat(path).await.is_ok_and(|s| s.is_dir()) {
        return Vec::new();
    }
    fs.read(path).await.unwrap_or_default()
}

/// The one line a symlink gets: where it points.
pub(crate) fn link(path: &str, target: &Path) -> String {
    format!("Symbolic link {path} -> {}\n", target.display())
}

/// `a<path>` -> `b<path>`; empty when both sides are empty, one line when
/// either side is binary.
pub(crate) fn unified(path: &str, before: &[u8], after: &[u8]) -> String {
    if before.is_empty() && after.is_empty() {
        return String::new();
    }
    if looks_binary(before) || looks_binary(after) {
        return format!("Binary file {path} differs\n");
    }
    let (old, new) = (
        String::from_utf8_lossy(before),
        String::from_utf8_lossy(after),
    );
    TextDiff::from_lines(old.as_ref(), new.as_ref())
        .unified_diff()
        .context_radius(CONTEXT)
        .header(&format!("a{path}"), &format!("b{path}"))
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_text_change_shows_both_lines_and_nothing_shows_nothing() {
        let d = unified("/f.txt", b"keep\nold line\n", b"keep\nnew line\n");
        assert!(d.lines().any(|l| l == "-old line"));
        assert!(d.lines().any(|l| l == "+new line"));
        assert_eq!(unified("/d", b"", b""), "");
    }

    #[test]
    fn an_added_file_is_all_additions() {
        let d = unified("/n.txt", b"", b"a\nb\n");
        let changed: Vec<_> = d
            .lines()
            .filter(|l| l.starts_with(['+', '-']) && !l.starts_with("+++") && !l.starts_with("---"))
            .collect();
        assert_eq!(changed, ["+a", "+b"]);
    }
}
