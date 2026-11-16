//! The built-in tool list: what [`builtin_tools`] makes from
//! [`BuiltinOptions`], and the words the `tools` setting picks them by.

use std::sync::Arc;

use haest_core::{task_board::TaskBoard, tool::Tool};

use crate::{
    BashOptions, BashTool, EditTool, GlobTool, GrepTool, ReadIntercept, ReadTool, SkillToolOptions,
    WebFetchNet, WebFetchTool, WriteTool, job_tools, skill_tool, task_tools,
};

/// What a host can change about the built-in tools. An option left None has
/// no tool: [`builtin_tools`] leaves it out, and [`builtin_tools_where`]
/// refuses a pick that turns its word on (a tool is never silently missing).
///
/// The default has WebFetch on the real network and no task tools or Skill.
#[derive(Clone)]
pub struct BuiltinOptions {
    /// PLUG POINT: asked before the workspace fs on every Read, so a host can
    /// answer some paths itself (an `attachment:<id>` that is no path at all).
    pub read_intercept: Option<Arc<dyn ReadIntercept>>,
    /// Bash's default timeout, and the job hub that adds background jobs
    /// (with BashOutput and KillShell).
    pub bash: BashOptions,
    /// WebFetch's resolver and HTTP client (a host with its own fetch, or no
    /// door to the network, leaves it None).
    pub web_fetch: Option<WebFetchNet>,
    /// The board the task tools work on (the host keeps it, to save it with
    /// the session).
    pub tasks: Option<Arc<TaskBoard>>,
    /// What the Skill tool reads.
    pub skill: Option<SkillToolOptions>,
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

/// Every built-in tool `opts` makes possible, in the order the model sees
/// them: Read, Write, Edit, Bash, then BashOutput and KillShell with a job hub
/// ([`BashOptions::jobs`]), Grep, Glob, then WebFetch, the four task tools and
/// Skill when their option is given.
///
/// The crate docs have an example; [`builtin_tools_where`] picks some.
pub fn builtin_tools(opts: BuiltinOptions) -> Vec<Arc<dyn Tool>> {
    let jobs = opts.bash.jobs.clone().map(job_tools).unwrap_or_default();
    let mut tools: Vec<Arc<dyn Tool>> = vec![
        Arc::new(ReadTool::new(opts.read_intercept)),
        Arc::new(WriteTool),
        Arc::new(EditTool),
        Arc::new(BashTool::new(opts.bash)),
    ];
    tools.extend(jobs);
    tools.extend([Arc::new(GrepTool) as Arc<dyn Tool>, Arc::new(GlobTool)]);
    if let Some(net) = opts.web_fetch {
        tools.push(Arc::new(WebFetchTool::new(net)));
    }
    tools.extend(opts.tasks.map(task_tools).unwrap_or_default());
    tools.extend(opts.skill.map(skill_tool));
    tools
}

/// The word that picks the built-in tool named `tool` (the `tools` setting):
/// `read`, `write`, `edit`, `bash` (with its job tools), `grep`, `glob`,
/// `web_fetch`, `tasks`, `skill`. None for any other name.
///
/// ```
/// use haest_tools::builtin_word;
///
/// assert_eq!(builtin_word("KillShell"), Some("bash"));
/// assert_eq!(builtin_word("TaskGet"), Some("tasks"));
/// assert_eq!(builtin_word("GenerateImage"), None);
/// ```
pub fn builtin_word(tool: &str) -> Option<&'static str> {
    Some(match tool {
        "Read" => "read",
        "Write" => "write",
        "Edit" => "edit",
        "Bash" | "BashOutput" | "KillShell" => "bash",
        "Grep" => "grep",
        "Glob" => "glob",
        "WebFetch" => "web_fetch",
        "TaskCreate" | "TaskUpdate" | "TaskList" | "TaskGet" => "tasks",
        "Skill" => "skill",
        _ => return None,
    })
}

/// A tool the `tools` setting turns on whose [`BuiltinOptions`] field is None:
/// what [`builtin_tools_where`] refuses, so a tool the host asked for is
/// never silently missing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("the tools setting has {word} on, but BuiltinOptions.{word} is None")]
pub struct MissingOption {
    /// The tool's word, the same as the option's field: `web_fetch`, `tasks`
    /// or `skill`.
    pub word: &'static str,
}

/// The built-in tools whose word ([`builtin_word`]) `on` takes, in
/// [`builtin_tools`]' order.
///
/// # Errors
///
/// [`MissingOption`] when `on` takes `web_fetch`, `tasks` or `skill` while the
/// option that tool needs is None.
///
/// # Examples
///
/// ```
/// use std::sync::Arc;
///
/// use haest_core::task_board::TaskBoard;
/// use haest_tools::{BuiltinOptions, builtin_tools_where};
///
/// let names = |tools: Vec<Arc<dyn haest_core::tool::Tool>>| {
///     tools.iter().map(|t| t.name().to_string()).collect::<Vec<_>>()
/// };
/// let read_only = |w: &str| matches!(w, "read" | "grep" | "glob");
/// let tools = builtin_tools_where(read_only, BuiltinOptions::default()).unwrap();
/// assert_eq!(names(tools), ["Read", "Grep", "Glob"]);
///
/// // The task tools need a board to work on.
/// let tasks = |w: &str| w == "tasks";
/// let missing = builtin_tools_where(tasks, BuiltinOptions::default()).err();
/// assert_eq!(missing.map(|m| m.word), Some("tasks"));
/// let opts = BuiltinOptions {
///     tasks: Some(Arc::new(TaskBoard::default())),
///     ..Default::default()
/// };
/// let tools = builtin_tools_where(tasks, opts).unwrap();
/// assert_eq!(names(tools), ["TaskCreate", "TaskUpdate", "TaskList", "TaskGet"]);
/// ```
pub fn builtin_tools_where(
    on: impl Fn(&str) -> bool,
    opts: BuiltinOptions,
) -> Result<Vec<Arc<dyn Tool>>, MissingOption> {
    let needs = [
        ("web_fetch", opts.web_fetch.is_some()),
        ("tasks", opts.tasks.is_some()),
        ("skill", opts.skill.is_some()),
    ];
    if let Some(&(word, _)) = needs.iter().find(|(word, given)| !given && on(word)) {
        return Err(MissingOption { word });
    }
    Ok(builtin_tools(opts)
        .into_iter()
        .filter(|t| builtin_word(t.name()).is_some_and(&on))
        .collect())
}

#[cfg(test)]
mod tests;
