//! System prompt assembly: identity, environment, instruction files, skills.
//!
//! Instruction files are read only when the host asks ([`ContextSources`]):
//! AGENTS.md (the cross-agent standard) and Claude Code's CLAUDE.md family.
//! User level first, then
//! every directory from the outermost project dir down to cwd, so the most
//! specific file is read last and wins on conflict.
//!
//! Nothing here reads the process: files come through a kern [`Fs`]; the home
//! dir, platform and date are the caller's.

mod git;
mod prompt;
#[cfg(test)]
mod tests;

use std::{
    collections::HashSet,
    path::{Path, PathBuf},
};

pub use git::current_git_branch;
pub use prompt::{PromptInputs, build_system_prompt};

use crate::fs::{Fs, normalize};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstructionFile {
    pub path: PathBuf,
    pub content: String,
}

/// The one name read without an opt-in; every other name below is Claude Code's.
const AGENTS_FILE: &str = "AGENTS.md";
const USER_FILES: [&str; 2] = ["CLAUDE.md", AGENTS_FILE];
const PROJECT_FILES: [&str; 4] = [
    AGENTS_FILE,
    "CLAUDE.md",
    ".claude/CLAUDE.md",
    "CLAUDE.local.md",
];
const MAX_FILE_CHARS: usize = 60_000;

async fn read_if_present(fs: &dyn Fs, path: &Path) -> Option<InstructionFile> {
    if !fs.exists(path).await {
        return None;
    }
    let bytes = fs.read(path).await.ok()?;
    let text = String::from_utf8_lossy(&bytes);
    let content = text.trim();
    if content.is_empty() {
        return None;
    }
    let content = match content.char_indices().nth(MAX_FILE_CHARS) {
        Some((cut, _)) => format!("{}\n[truncated]", &content[..cut]),
        None => content.to_string(),
    };
    Some(InstructionFile {
        path: path.to_path_buf(),
        content,
    })
}

/// Directories from the outermost ancestor below `home` (or the fs root) down to cwd.
fn project_chain(cwd: &Path, home: &Path) -> Vec<PathBuf> {
    let mut chain = Vec::new();
    let mut cur = normalize(cwd);
    while cur != home {
        let parent = cur.parent().map(Path::to_path_buf);
        chain.insert(0, cur);
        match parent {
            Some(p) => cur = p,
            None => break,
        }
    }
    chain
}

crate::word_set! {
    /// Which instruction files kern reads (`context`). None unless the host says so.
    pub ContextSources {
        /// `AGENTS.md`: `~/.agents/AGENTS.md`, then every dir down to cwd.
        agents,
        /// Claude Code's: `CLAUDE.md`, `.claude/CLAUDE.md`, `CLAUDE.local.md`
        /// and the user's `~/.claude/CLAUDE.md`.
        claude,
    }
}

/// Where to look.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstructionOpts {
    pub cwd: PathBuf,
    pub home: PathBuf,
    /// Claude Code's user dir (`<home>/.claude` when None); read only with `claude`.
    pub config_dir: Option<PathBuf>,
    pub sources: ContextSources,
}

/// Every path to try, in reading order: user dirs, then the project chain.
fn candidates(opts: &InstructionOpts) -> Vec<PathBuf> {
    let ContextSources { agents, claude } = opts.sources;
    let wanted = |f: &str| if f == AGENTS_FILE { agents } else { claude };
    let mut user_dirs = Vec::new();
    if claude {
        let dir = opts.config_dir.clone();
        user_dirs.push(dir.unwrap_or_else(|| opts.home.join(".claude")));
    }
    let agents_dir = opts.home.join(".agents");
    if agents && !user_dirs.contains(&agents_dir) {
        user_dirs.push(agents_dir);
    }
    let dirs = [
        (user_dirs, &USER_FILES[..]),
        (project_chain(&opts.cwd, &opts.home), &PROJECT_FILES[..]),
    ];
    let mut out = Vec::new();
    for (dirs, names) in dirs {
        for dir in dirs {
            let wanted_names = names.iter().copied().filter(|f| wanted(f));
            out.extend(wanted_names.map(|f| dir.join(f)));
        }
    }
    out
}

pub async fn find_instruction_files(fs: &dyn Fs, opts: &InstructionOpts) -> Vec<InstructionFile> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for path in candidates(opts) {
        if !seen.insert(path.clone()) {
            continue;
        }
        if let Some(file) = read_if_present(fs, &path).await {
            out.push(file);
        }
    }
    out
}
