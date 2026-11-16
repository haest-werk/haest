//! The system prompt: identity, how to use the tools this call has, the
//! environment, the workspace's limits, instruction files, skills, and what the
//! host appends. An empty section is left out.

use super::InstructionFile;
use crate::{
    skills::{Skill, skills_prompt_block},
    tool::FILE_TOOLS,
};

/// Everything the system prompt says, gathered by the caller.
#[derive(Debug, Clone, Default)]
pub struct PromptInputs {
    /// Replaces haest's identity paragraph (a host's `system_prompt` setting).
    pub identity: Option<String>,
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
    /// What the workspace can and cannot do (the sandbox's limits); None for
    /// the real one. Said only to a call with Bash or a file tool.
    pub workspace_note: Option<String>,
    pub append_system_prompt: Option<String>,
}

const IDENTITY: &str = "You are haest, a coding agent. You work inside the user's project through tools: read before you edit, verify what you change, and keep going until the task is done. Be direct and concise; lead with the answer. When you are unsure, investigate with tools instead of guessing.";

/// `[A]` -> "A", `[A, B]` -> "A and B", `[A, B, C]` -> "A, B and C".
fn and_list(names: &[&str]) -> String {
    match names {
        [] => String::new(),
        [one] => one.to_string(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
}

/// One line per kind of tool this call has; nothing when it has none of them.
fn tool_discipline(tools: &[String]) -> String {
    let has = |n: &str| tools.iter().any(|t| t == n);
    let look: Vec<&str> = ["Read", "Grep", "Glob"]
        .into_iter()
        .filter(|n| has(n))
        .collect();
    let bash = "Bash for builds, tests, git and anything else";
    let mut lines = Vec::new();
    match (look.is_empty(), has("Bash")) {
        (false, true) => lines.push(format!(
            "- Use {} to look at code; use {bash}.",
            and_list(&look)
        )),
        (false, false) => lines.push(format!("- Use {} to look at code.", and_list(&look))),
        (true, true) => lines.push(format!("- Use {bash}.")),
        (true, false) => {}
    }
    match (has("Edit"), has("Write")) {
        (true, true) => lines.push("- Use Edit for changes to existing files (exact old_string match); Write only for new files or full rewrites.".into()),
        (true, false) => lines.push("- Use Edit for changes to existing files (exact old_string match).".into()),
        (false, true) => lines.push("- Use Write for new files or full rewrites.".into()),
        (false, false) => {}
    }
    if FILE_TOOLS.iter().any(|n| has(n)) {
        lines.push("- Paths may be absolute or relative to the working directory.".into());
    }
    match lines.is_empty() {
        true => String::new(),
        false => format!("Tool discipline:\n{}", lines.join("\n")),
    }
}

fn instructions(files: &[InstructionFile]) -> String {
    if files.is_empty() {
        return String::new();
    }
    let head = [
        "# Instructions".to_string(),
        "The user and project gave these instructions. Follow them; they override defaults above."
            .into(),
    ];
    let files = files
        .iter()
        .map(|f| format!("## {}\n\n{}", f.path.display(), f.content));
    head.into_iter()
        .chain(files)
        .collect::<Vec<_>>()
        .join("\n\n")
}

pub fn build_system_prompt(p: &PromptInputs) -> String {
    let branch = match &p.git_branch {
        Some(b) => format!("- Git branch: {b}"),
        None => "- Not a git repository".into(),
    };
    let tools = match p.tool_names.is_empty() {
        true => "none".to_string(),
        false => p.tool_names.join(", "),
    };
    let env = [
        "# Environment".to_string(),
        format!("- Working directory: {}", p.cwd),
        format!("- Platform: {}", p.platform),
        format!("- Date: {}", p.date),
        format!("- Model: {}", p.model),
        branch,
        format!("- Tools: {tools}"),
    ]
    .join("\n");
    let opt = |s: &Option<String>| s.clone().unwrap_or_default();
    let identity = p.identity.clone().unwrap_or_else(|| IDENTITY.to_string());
    // The workspace's limits only matter to a call that can touch it.
    let touches_workspace = p
        .tool_names
        .iter()
        .any(|t| t == "Bash" || FILE_TOOLS.contains(&t.as_str()));
    let workspace_note = match touches_workspace {
        true => opt(&p.workspace_note),
        false => String::new(),
    };
    [
        identity,
        tool_discipline(&p.tool_names),
        env,
        workspace_note,
        instructions(&p.instructions),
        skills_prompt_block(&p.skills),
        opt(&p.channel_guide),
        opt(&p.append_system_prompt),
    ]
    .into_iter()
    .filter(|s| !s.trim().is_empty())
    .collect::<Vec<_>>()
    .join("\n\n")
}
