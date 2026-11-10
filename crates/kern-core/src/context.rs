//! System prompt assembly: identity, environment, instruction files, skills.
//!
//! Instruction files are AGENTS.md (the cross-agent standard); Claude Code's
//! CLAUDE.md family is read only when the host opts in
//! ([`InstructionOpts::claude_files`]). User level first, then every directory
//! from the outermost project dir down to cwd, so the most specific file is
//! read last and wins on conflict.
//!
//! Nothing here reads the process: files come through a kern [`Fs`]; the home
//! dir, platform and date are the caller's.

mod git;
#[cfg(test)]
mod tests;

use std::{
    collections::HashSet,
    path::{Path, PathBuf},
};

pub use git::current_git_branch;

use crate::{
    fs::{Fs, normalize},
    skills::{Skill, skills_prompt_block},
};

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

/// Where to look.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstructionOpts {
    pub cwd: PathBuf,
    pub home: PathBuf,
    /// Claude Code's user dir (`<home>/.claude` when None); read only with `claude_files`.
    pub config_dir: Option<PathBuf>,
    /// Also read Claude Code's files: `CLAUDE.md`, `.claude/CLAUDE.md`,
    /// `CLAUDE.local.md` and the user's `~/.claude/CLAUDE.md`. Off = AGENTS.md only.
    pub claude_files: bool,
}

/// Every path to try, in reading order: user dirs, then the project chain.
fn candidates(opts: &InstructionOpts) -> Vec<PathBuf> {
    let wanted = |f: &str| opts.claude_files || f == AGENTS_FILE;
    let mut user_dirs = Vec::new();
    if opts.claude_files {
        let claude = opts.config_dir.clone();
        user_dirs.push(claude.unwrap_or_else(|| opts.home.join(".claude")));
    }
    let agents = opts.home.join(".agents");
    if !user_dirs.contains(&agents) {
        user_dirs.push(agents);
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

/// Everything the system prompt says, gathered by the caller.
#[derive(Debug, Clone, Default)]
pub struct PromptInputs {
    pub cwd: String,
    pub model: String,
    pub platform: String,
    pub date: String,
    pub git_branch: Option<String>,
    pub instructions: Vec<InstructionFile>,
    pub skills: Vec<Skill>,
    pub tool_names: Vec<String>,
    /// How to read and answer `<channel>` turns; None when there is no way to answer.
    pub channel_guide: Option<String>,
    /// What the workspace can and cannot do (the sandbox's limits); None for the real one.
    pub workspace_note: Option<String>,
    pub append_system_prompt: Option<String>,
}

const IDENTITY: &str = "You are kern, a coding agent. You work inside the user's project through tools: read before you edit, verify what you change, and keep going until the task is done. Be direct and concise; lead with the answer. When you are unsure, investigate with tools instead of guessing.

Tool discipline:
- Use Read, Grep and Glob to look at code; use Bash for builds, tests, git and anything else.
- Use Edit for changes to existing files (exact old_string match); Write only for new files or full rewrites.
- Paths may be absolute or relative to the working directory.";

pub fn build_system_prompt(p: &PromptInputs) -> String {
    let branch = match &p.git_branch {
        Some(b) => format!("- Git branch: {b}"),
        None => "- Not a git repository".into(),
    };
    let env = [
        "# Environment".to_string(),
        format!("- Working directory: {}", p.cwd),
        format!("- Platform: {}", p.platform),
        format!("- Date: {}", p.date),
        format!("- Model: {}", p.model),
        branch,
        format!("- Tools: {}", p.tool_names.join(", ")),
    ]
    .join("\n");
    let instructions = if p.instructions.is_empty() {
        String::new()
    } else {
        let head = [
            "# Instructions".to_string(),
            "The user and project gave these instructions. Follow them; they override defaults above.".into(),
        ];
        let files = p
            .instructions
            .iter()
            .map(|f| format!("## {}\n\n{}", f.path.display(), f.content));
        head.into_iter()
            .chain(files)
            .collect::<Vec<_>>()
            .join("\n\n")
    };
    let opt = |s: &Option<String>| s.clone().unwrap_or_default();
    [
        IDENTITY.to_string(),
        env,
        opt(&p.workspace_note),
        instructions,
        skills_prompt_block(&p.skills),
        opt(&p.channel_guide),
        opt(&p.append_system_prompt),
    ]
    .into_iter()
    .filter(|s| !s.trim().is_empty())
    .collect::<Vec<_>>()
    .join("\n\n")
}
