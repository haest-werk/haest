//! The built-in tool list: what [`builtin_tools`] makes from
//! [`BuiltinOptions`], and the words the `tools` setting picks them by
//! ([`BuiltinTool`]).

use std::{fmt, str::FromStr};

use haest_core::{ToolRef, Tools, task_board::TaskBoard};

use crate::{
    BashOptions, BashTool, EditTool, GlobTool, GrepTool, ReadInterceptRef, ReadTool, SkillTool,
    SkillToolOptions, WebFetchNet, WebFetchTool, WriteTool, job_tools, task_tools,
};

/// A built-in tool, by the word the `tools` setting picks it with: `read`,
/// `write`, `edit`, `bash` (with its job tools), `grep`, `glob`,
/// `web_fetch`, `tasks` (the four task tools) and `skill`.
///
/// It reads and parses as its word.
///
/// ```
/// use haest_tools::BuiltinTool;
///
/// assert_eq!(BuiltinTool::of("KillShell"), Some(BuiltinTool::Bash));
/// assert_eq!(BuiltinTool::of("TaskGet"), Some(BuiltinTool::Tasks));
/// assert_eq!(BuiltinTool::of("GenerateImage"), None);
/// assert_eq!(BuiltinTool::WebFetch.to_string(), "web_fetch");
/// assert_eq!("web_fetch".parse(), Ok(BuiltinTool::WebFetch));
/// assert!("fetch".parse::<BuiltinTool>().is_err());
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[non_exhaustive]
pub enum BuiltinTool {
    /// Read.
    Read,
    /// Write.
    Write,
    /// Edit.
    Edit,
    /// Bash, and BashOutput and KillShell with a job hub.
    Bash,
    /// Grep.
    Grep,
    /// Glob.
    Glob,
    /// WebFetch.
    WebFetch,
    /// TaskCreate, TaskUpdate, TaskList and TaskGet.
    Tasks,
    /// Skill.
    Skill,
}

impl BuiltinTool {
    /// Every built-in, in [`builtin_tools`]' order.
    pub const ALL: &'static [BuiltinTool] = &[
        Self::Read,
        Self::Write,
        Self::Edit,
        Self::Bash,
        Self::Grep,
        Self::Glob,
        Self::WebFetch,
        Self::Tasks,
        Self::Skill,
    ];

    /// Its word in the `tools` setting.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Read => "read",
            Self::Write => "write",
            Self::Edit => "edit",
            Self::Bash => "bash",
            Self::Grep => "grep",
            Self::Glob => "glob",
            Self::WebFetch => "web_fetch",
            Self::Tasks => "tasks",
            Self::Skill => "skill",
        }
    }

    /// The built-in the tool named `tool` belongs to; None for any other
    /// name.
    pub fn of(tool: &str) -> Option<Self> {
        Some(match tool {
            "Read" => Self::Read,
            "Write" => Self::Write,
            "Edit" => Self::Edit,
            "Bash" | "BashOutput" | "KillShell" => Self::Bash,
            "Grep" => Self::Grep,
            "Glob" => Self::Glob,
            "WebFetch" => Self::WebFetch,
            "TaskCreate" | "TaskUpdate" | "TaskList" | "TaskGet" => Self::Tasks,
            "Skill" => Self::Skill,
            _ => return None,
        })
    }
}

impl fmt::Display for BuiltinTool {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for BuiltinTool {
    type Err = ParseBuiltinToolError;

    fn from_str(word: &str) -> Result<Self, Self::Err> {
        Self::ALL
            .iter()
            .copied()
            .find(|b| b.as_str() == word)
            .ok_or_else(|| ParseBuiltinToolError {
                word: word.to_string(),
            })
    }
}

/// A word that names no [`BuiltinTool`].
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error(
    "\"{word}\" is not a built-in tool: one of read, write, edit, bash, grep, glob, web_fetch, tasks, skill"
)]
pub struct ParseBuiltinToolError {
    word: String,
}

impl ParseBuiltinToolError {
    /// The word, as given.
    pub fn word(&self) -> &str {
        &self.word
    }
}

/// What a host can change about the built-in tools. An option it does not
/// give has no tool: [`builtin_tools`] leaves it out, and
/// [`builtin_tools_where`] refuses a pick that turns its word on (a tool is
/// never silently missing).
///
/// The default has WebFetch on the real network and no task tools or Skill.
///
/// ```
/// use haest_core::task_board::TaskBoard;
/// use haest_tools::{BuiltinOptions, builtin_tools};
///
/// let opts = BuiltinOptions::new().with_tasks(TaskBoard::default()).without_web_fetch();
/// let names: Vec<String> = builtin_tools(opts).iter().map(|t| t.name().to_string()).collect();
/// assert!(names.contains(&"TaskList".to_string()) && !names.contains(&"WebFetch".to_string()));
/// ```
#[derive(Debug, Clone)]
pub struct BuiltinOptions {
    read_intercept: Option<ReadInterceptRef>,
    bash: BashOptions,
    web_fetch: Option<WebFetchNet>,
    tasks: Option<TaskBoard>,
    skill: Option<SkillToolOptions>,
}

impl Default for BuiltinOptions {
    fn default() -> Self {
        Self {
            read_intercept: None,
            bash: BashOptions::default(),
            web_fetch: Some(WebFetchNet::default()),
            tasks: None,
            skill: None,
        }
    }
}

impl BuiltinOptions {
    /// The default: WebFetch on the real network, no task tools, no Skill.
    pub fn new() -> Self {
        Self::default()
    }

    /// PLUG POINT: asked before the workspace fs on every Read, so a host can
    /// answer some paths itself (an `attachment:<id>` that is no path at
    /// all). [`ReadIntercept`](crate::ReadIntercept) has an example.
    pub fn with_read_intercept(mut self, intercept: impl Into<ReadInterceptRef>) -> Self {
        self.read_intercept = Some(intercept.into());
        self
    }

    /// Bash's default timeout, and the job hub that adds background jobs
    /// (with BashOutput and KillShell).
    pub fn with_bash(mut self, bash: BashOptions) -> Self {
        self.bash = bash;
        self
    }

    /// WebFetch over `net`: a host with its own resolver or client.
    pub fn with_web_fetch(mut self, net: WebFetchNet) -> Self {
        self.web_fetch = Some(net);
        self
    }

    /// No WebFetch: a host with its own fetch, or no door to the network.
    pub fn without_web_fetch(mut self) -> Self {
        self.web_fetch = None;
        self
    }

    /// The task tools, over `board` (the host keeps a clone, to show the
    /// list and save it with the session).
    pub fn with_tasks(mut self, board: TaskBoard) -> Self {
        self.tasks = Some(board);
        self
    }

    /// The Skill tool, reading what `skill` offers.
    pub fn with_skill(mut self, skill: SkillToolOptions) -> Self {
        self.skill = Some(skill);
        self
    }

    /// Whether the option `tool` needs is given; the ones that need none
    /// always are.
    fn gives(&self, tool: BuiltinTool) -> bool {
        match tool {
            BuiltinTool::WebFetch => self.web_fetch.is_some(),
            BuiltinTool::Tasks => self.tasks.is_some(),
            BuiltinTool::Skill => self.skill.is_some(),
            _ => true,
        }
    }
}

/// Every built-in tool `opts` makes possible, in the order the model sees
/// them: Read, Write, Edit, Bash, then BashOutput and KillShell with a job hub
/// ([`BashOptions::with_jobs`]), Grep, Glob, then WebFetch, the four task tools and
/// Skill when their option is given.
///
/// The crate docs have an example; [`builtin_tools_where`] picks some.
pub fn builtin_tools(opts: BuiltinOptions) -> Tools {
    let jobs = opts.bash.jobs().cloned().map(job_tools).unwrap_or_default();
    let mut tools: Tools = Tools::from_iter([
        ToolRef::new(ReadTool::intercepted_by(opts.read_intercept)),
        ToolRef::new(WriteTool::new()),
        ToolRef::new(EditTool::new()),
        ToolRef::new(BashTool::new(opts.bash)),
    ]);
    tools.extend(jobs);
    tools.extend([ToolRef::new(GrepTool::new()), ToolRef::new(GlobTool::new())]);
    if let Some(net) = opts.web_fetch {
        tools.push(WebFetchTool::new(net));
    }
    tools.extend(opts.tasks.map(task_tools).unwrap_or_default());
    tools.extend(opts.skill.map(SkillTool::new));
    tools
}

/// A built-in the `tools` setting turns on whose option [`BuiltinOptions`]
/// does not give: what [`builtin_tools_where`] refuses, so a tool the host
/// asked for is never silently missing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("the tools setting has {tool} on, but BuiltinOptions gives none: give it with_{tool}")]
#[non_exhaustive]
pub struct MissingOption {
    /// The tool: [`WebFetch`](BuiltinTool::WebFetch),
    /// [`Tasks`](BuiltinTool::Tasks) or [`Skill`](BuiltinTool::Skill).
    pub tool: BuiltinTool,
}

/// The built-in tools `on` takes, in [`builtin_tools`]' order.
///
/// # Errors
///
/// [`MissingOption`] when `on` takes WebFetch, the task tools or Skill while
/// `opts` does not give the option that tool needs.
///
/// # Examples
///
/// ```
/// use haest_core::task_board::TaskBoard;
/// use haest_tools::{BuiltinOptions, BuiltinTool, MissingOption, builtin_tools_where};
///
/// let names = |tools: haest_core::tool::Tools| {
///     tools.iter().map(|t| t.name().to_string()).collect::<Vec<_>>()
/// };
/// let read_only = |t| matches!(t, BuiltinTool::Read | BuiltinTool::Grep | BuiltinTool::Glob);
/// let tools = builtin_tools_where(read_only, BuiltinOptions::default())?;
/// assert_eq!(names(tools), ["Read", "Grep", "Glob"]);
///
/// // The task tools need a board to work on.
/// let tasks = |t| t == BuiltinTool::Tasks;
/// let missing = builtin_tools_where(tasks, BuiltinOptions::default()).err();
/// assert_eq!(missing.map(|m| m.tool), Some(BuiltinTool::Tasks));
/// let opts = BuiltinOptions::new().with_tasks(TaskBoard::default());
/// let tools = builtin_tools_where(tasks, opts)?;
/// assert_eq!(names(tools), ["TaskCreate", "TaskUpdate", "TaskList", "TaskGet"]);
/// # Ok::<(), MissingOption>(())
/// ```
pub fn builtin_tools_where(
    on: impl Fn(BuiltinTool) -> bool,
    opts: BuiltinOptions,
) -> Result<Tools, MissingOption> {
    let missing = BuiltinTool::ALL.iter().copied();
    if let Some(tool) = missing.filter(|&t| on(t)).find(|&t| !opts.gives(t)) {
        return Err(MissingOption { tool });
    }
    Ok(builtin_tools(opts)
        .into_iter()
        .filter(|t| BuiltinTool::of(t.name()).is_some_and(&on))
        .collect())
}

#[cfg(test)]
mod tests;
