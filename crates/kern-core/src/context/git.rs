//! The checked-out git branch, read from HEAD through a kern Fs -- no git process.

use std::path::{Path, PathBuf};

use crate::fs::{Fs, normalize, read_lossy, resolve_path};

/// `gitdir: <path>` on any line of a worktree's `.git` file.
fn gitdir_line(text: &str) -> Option<&str> {
    text.lines()
        .filter_map(|l| l.strip_prefix("gitdir:"))
        .map(str::trim)
        .find(|p| !p.is_empty())
}

/// The git dir for `cwd`: the nearest `.git` dir, or where a worktree's `.git` file points.
async fn find_git_dir(fs: &dyn Fs, cwd: &Path) -> Option<PathBuf> {
    let mut dir = normalize(cwd);
    loop {
        let dot_git = dir.join(".git");
        if let Ok(st) = fs.stat(&dot_git).await {
            if st.is_dir() {
                return Some(dot_git);
            }
            if st.is_file() {
                let text = read_lossy(fs, &dot_git).await?;
                return gitdir_line(&text).map(|p| resolve_path(&dir, p));
            }
        }
        dir = dir.parent()?.to_path_buf();
    }
}

/// `ref: refs/heads/<branch>` -> the branch.
fn branch_of(head: &str) -> Option<&str> {
    head.strip_prefix("ref:")?
        .trim_start()
        .strip_prefix("refs/heads/")
        .filter(|b| !b.is_empty() && !b.contains('\n'))
}

/// The checked-out branch (or a short sha when detached), read from HEAD; None outside a repo.
pub async fn current_git_branch(fs: &dyn Fs, cwd: &Path) -> Option<String> {
    let git_dir = find_git_dir(fs, cwd).await?;
    let head = read_lossy(fs, &git_dir.join("HEAD")).await?;
    let head = head.trim();
    match branch_of(head) {
        Some(b) => Some(b.to_string()),
        None => Some(head.chars().take(12).collect::<String>()).filter(|s| !s.is_empty()),
    }
}
