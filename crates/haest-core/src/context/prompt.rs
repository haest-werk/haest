//! The system prompt: identity, how to use the tools this call has, the
//! environment, the workspace's limits, instruction files, skills, and what the
//! host appends. An empty section is left out.

use super::InstructionFile;
use crate::{
    ToolRef,
    skills::{Skill, skills_prompt_block},
};

/// Everything the system prompt says, gathered by the caller.
///
/// `Default` is empty: fill in what the host knows and leave the rest.
#[derive(Debug, Clone, Default)]
pub struct PromptInputs {
    /// Replaces haest's identity paragraph (a host's `system_prompt` setting).
    pub identity: Option<String>,
    /// The working directory, as the model should read it.
    pub cwd: String,
    /// The model, as the model should read it.
    pub model_spec: String,
    /// The platform, e.g. `linux` or `darwin`.
    pub platform: String,
    /// Today's date, in whatever form the host likes.
    pub date: String,
    /// The checked-out branch ([`current_git_branch`](crate::context::current_git_branch));
    /// None says "Not a git repository".
    pub git_branch: Option<String>,
    /// Instruction files, in reading order ([`find_instruction_files`](crate::context::find_instruction_files)).
    pub instructions: Vec<InstructionFile>,
    /// The skills the model may load.
    pub skills: Vec<Skill>,
    /// The names of the tools this call advertises, listed in the environment.
    pub tool_names: Vec<String>,
    /// How to use those tools, said right after the identity; None says
    /// nothing. [`tool_guide`] gathers it from the tools themselves.
    pub tool_guide: Option<String>,
    /// How to read and answer `<channel>` turns; None when there is no way to answer.
    pub channel_guide: Option<String>,
    /// What the workspace can and cannot do (the sandbox's limits); None for
    /// the real one, and for a call with no tool that touches it (haest-tools'
    /// `touches_workspace`).
    pub workspace_note: Option<String>,
    /// Text the host adds at the very end.
    pub append_system_prompt: Option<String>,
}

/// The tool guide for a call offered `tools`
/// ([`PromptInputs::tool_guide`]): each tool's
/// [`prompt_guide`](crate::Tool::prompt_guide) line, in the list's order,
/// under `Tool discipline:`. None when no tool has a line.
///
/// ```
/// # use async_trait::async_trait;
/// # use haest_core::{JsonMap, Tool, ToolContext, ToolError, ToolOutput, ToolRef};
/// # use serde_json::{Value, json};
/// use haest_core::{Tools, context::tool_guide};
///
/// struct Search;
///
/// #[async_trait]
/// impl Tool for Search {
///     fn name(&self) -> &str { "Search" }
///     fn description(&self) -> String { "Search the notes.".into() }
///     fn prompt_guide(&self) -> Option<String> {
///         Some("Use Search before you answer from memory.".into())
///     }
/// #   fn parameters(&self) -> Value { json!({"type": "object"}) }
/// #   fn kind(&self) -> &str { "notes.search" }
/// #   async fn execute(&self, _: JsonMap, _: &ToolContext) -> Result<ToolOutput, ToolError> {
/// #       Ok(ToolOutput::text(""))
/// #   }
/// }
///
/// let guide = tool_guide(&Tools::new().with(Search));
/// assert_eq!(guide.as_deref(), Some("Tool discipline:\n- Use Search before you answer from memory."));
/// assert_eq!(tool_guide(&Tools::new()), None);
/// ```
pub fn tool_guide(tools: &[ToolRef]) -> Option<String> {
    let lines: Vec<String> = tools
        .iter()
        .filter_map(|t| t.prompt_guide())
        .filter(|line| !line.trim().is_empty())
        .map(|line| format!("- {}", line.trim()))
        .collect();
    (!lines.is_empty()).then(|| format!("Tool discipline:\n{}", lines.join("\n")))
}

const IDENTITY: &str = "You are haest, a coding agent. You work inside the user's project through tools: read before you edit, verify what you change, and keep going until the task is done. Be direct and concise; lead with the answer. When you are unsure, investigate with tools instead of guessing.";

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

fn environment(p: &PromptInputs) -> String {
    let branch = match &p.git_branch {
        Some(b) => format!("- Git branch: {b}"),
        None => "- Not a git repository".into(),
    };
    let tools = if p.tool_names.is_empty() {
        "none".to_string()
    } else {
        p.tool_names.join(", ")
    };
    [
        "# Environment".to_string(),
        format!("- Working directory: {}", p.cwd),
        format!("- Platform: {}", p.platform),
        format!("- Date: {}", p.date),
        format!("- Model: {}", p.model_spec),
        branch,
        format!("- Tools: {tools}"),
    ]
    .join("\n")
}

/// The system prompt's sections, each one's text built, in the order the
/// prompt says them. An empty section says nothing: [`join`](Self::join)
/// leaves it out.
///
/// The parts a prompt template reorders, wraps or drops without copying
/// haest's text: it serializes with these field names (`sections.tools`).
///
/// ```
/// use haest_core::context::{PromptInputs, PromptSections, build_system_prompt};
///
/// let inputs = PromptInputs { cwd: "/work".into(), ..PromptInputs::default() };
/// let sections = PromptSections::new(&inputs);
/// assert!(sections.identity.starts_with("You are haest"));
/// assert!(sections.environment.contains("- Working directory: /work"));
/// assert_eq!(sections.workspace, "", "no note: the section is empty");
/// assert_eq!(sections.join(), build_system_prompt(&inputs));
/// ```
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize)]
#[non_exhaustive]
pub struct PromptSections {
    /// Who the model is: haest's paragraph, or the host's
    /// ([`PromptInputs::identity`]).
    pub identity: String,
    /// How to use the tools this call offers ([`PromptInputs::tool_guide`]).
    pub tools: String,
    /// `# Environment`: working directory, platform, date, model, git branch,
    /// tool names.
    pub environment: String,
    /// What the workspace can and cannot do ([`PromptInputs::workspace_note`]).
    pub workspace: String,
    /// `# Instructions`: every instruction file, in reading order.
    pub instructions: String,
    /// The skills the model may load.
    pub skills: String,
    /// How to read and answer `<channel>` turns ([`PromptInputs::channel_guide`]).
    pub channel: String,
    /// The host's text at the very end ([`PromptInputs::append_system_prompt`]).
    pub append: String,
}

impl PromptSections {
    /// Every section of the prompt `p` describes.
    pub fn new(p: &PromptInputs) -> Self {
        let opt = |s: &Option<String>| s.clone().unwrap_or_default();
        Self {
            identity: p.identity.clone().unwrap_or_else(|| IDENTITY.to_string()),
            tools: opt(&p.tool_guide),
            environment: environment(p),
            workspace: opt(&p.workspace_note),
            instructions: instructions(&p.instructions),
            skills: skills_prompt_block(&p.skills),
            channel: opt(&p.channel_guide),
            append: opt(&p.append_system_prompt),
        }
    }

    /// The prompt text: the sections in order, the empty ones left out,
    /// separated by a blank line.
    pub fn join(&self) -> String {
        [
            &self.identity,
            &self.tools,
            &self.environment,
            &self.workspace,
            &self.instructions,
            &self.skills,
            &self.channel,
            &self.append,
        ]
        .into_iter()
        .filter(|s| !s.trim().is_empty())
        .map(String::as_str)
        .collect::<Vec<_>>()
        .join("\n\n")
    }
}

/// The system prompt text, sections in this order: identity, tool guide,
/// environment, workspace note, instruction files, skills, channel guide,
/// the host's addition. Empty sections are left out; sections are separated
/// by a blank line. [`PromptSections`] holds them before they are joined.
pub fn build_system_prompt(p: &PromptInputs) -> String {
    PromptSections::new(p).join()
}
