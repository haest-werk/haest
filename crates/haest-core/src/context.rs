//! System prompt assembly: identity, environment, instruction files, skills.
//!
//! Instruction files are read only when the host asks ([`ContextSources`]):
//! AGENTS.md (the cross-agent standard) and Claude Code's CLAUDE.md family.
//! User level first, then
//! every directory from the outermost project dir down to cwd, so the most
//! specific file is read last and wins on conflict.
//!
//! Nothing here reads the process: files come through a haest [`Fs`]; the home
//! dir, platform and date are the caller's.
//!
//! A host gathers [`PromptInputs`] ([`find_instruction_files`],
//! [`current_git_branch`], the skills it loaded, the tool names) and builds
//! the text with [`build_system_prompt`], typically inside its
//! [`SystemPrompt`](crate::SystemPrompt) so it is rebuilt before every call.
//!
//! ```
//! # #[tokio::main(flavor = "current_thread")]
//! # async fn main() {
//! use haest_core::{
//!     MemFs,
//!     context::{
//!         ContextSources, InstructionOpts, PromptInputs, build_system_prompt,
//!         find_instruction_files,
//!     },
//! };
//!
//! let fs = MemFs::with_files([("/home/me/work/AGENTS.md", "Run the tests before you commit.")]);
//!
//! let opts = InstructionOpts {
//!     cwd: "/home/me/work".into(),
//!     home: "/home/me".into(),
//!     config_dir: None,
//!     sources: ContextSources { agents: true, claude: false },
//! };
//! let instructions = find_instruction_files(&fs, &opts).await;
//! assert_eq!(instructions.len(), 1);
//!
//! let prompt = build_system_prompt(&PromptInputs {
//!     cwd: "/home/me/work".into(),
//!     model: "anthropic:claude-x".into(),
//!     platform: "linux".into(),
//!     date: "2026-01-01".into(),
//!     instructions,
//!     tool_names: vec!["Read".into(), "Bash".into()],
//!     ..PromptInputs::default()
//! });
//! assert!(prompt.contains("Run the tests before you commit."));
//! # }
//! ```

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

use crate::fs::{Fs, normalize, read_lossy};

/// One instruction file that was found and read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstructionFile {
    /// Where it was read from; the prompt names it.
    pub path: PathBuf,
    /// Its text, trimmed, cut at 60,000 characters (with a `[truncated]` mark).
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
    let text = read_lossy(fs, path).await?;
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

/// The dirs a project's files may sit in: from the outermost ancestor below
/// `home` (or the fs root) down to `cwd`. Never `home` itself: its files are
/// the user's, not a project's.
pub fn project_chain(cwd: &Path, home: &Path) -> Vec<PathBuf> {
    let home = normalize(home);
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
    /// Which instruction files haest reads (`context`). None unless the host says so.
    pub ContextSources {
        /// `AGENTS.md`: `~/.agents/AGENTS.md`, then every dir down to cwd.
        agents,
        /// Claude Code's: `CLAUDE.md`, `.claude/CLAUDE.md`, `CLAUDE.local.md`
        /// and the user's `~/.claude/CLAUDE.md`.
        claude,
    }
}

/// Where [`find_instruction_files`] looks, and for which files.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstructionOpts {
    /// The working directory: the innermost project dir.
    pub cwd: PathBuf,
    /// The user's home: its `.agents` / `.claude` dirs hold the user's files,
    /// and the project chain stops below it ([`project_chain`]).
    pub home: PathBuf,
    /// Claude Code's user dir (`<home>/.claude` when None); read only with `claude`.
    pub config_dir: Option<PathBuf>,
    /// Which kinds of instruction file to read; none unless the host says so.
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

/// Every instruction file `opts` asks for that exists and is not empty, in
/// reading order: the user's files first, then each project dir from the
/// outermost down to `cwd`. Each path is read once; a file that cannot be
/// read is skipped, so this never fails.
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
