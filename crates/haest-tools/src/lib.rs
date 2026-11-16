//! haest's built-in tools. PORT (T7): `tools/*.ts`.
//!
//! Every tool here touches files and commands ONLY through `ctx.workspace`
//! (`fs()`, `shell`, `run`), so the sandbox's in-memory files and the real disk
//! look the same to them. WebFetch is the one exception: it runs in the HOST,
//! never through the workspace, behind the net guard ([`net_guard`]).
//!
//! | Tool | Kind | Module |
//! |---|---|---|
//! | Read | `file.read` | [`read`] (PLUG POINT: [`read::ReadIntercept`]) |
//! | Write, Edit | `file.write`, `file.edit` | [`files`] |
//! | Glob, Grep | `file.glob`, `text.search` | [`search`] over [`fs_walk`] |
//! | Bash | `shell.exec` | [`bash`] |
//! | BashOutput, KillShell | `shell.exec` | [`bash_jobs`] over a [`jobs::JobHub`] the host owns |
//! | WebFetch | `web.fetch` | [`web_fetch`] (PLUG POINT: [`web_fetch::WebFetchNet`]) |
//! | Agent, SendMessage, StopAgent | `task.spawn`, `agent.unknown` | [`agents`] (PLUG POINT: [`agents::AgentDirectory`]; default [`agents::SubagentHub`]) |
//! | GenerateImage | `image.generate` | [`image`] (PLUG POINT: [`image::ImageHttp`]; the key is the host's) |
//!
//! A turn the human did not start (a job that exited, a subagent's message for
//! main) reaches the model through the host's [`inbox::Inbox`], counted by an
//! [`turn_gate::AgentTurnGate`].

use std::sync::Arc;

use haest_core::{task_board::TaskBoard, tool::Tool};

pub mod agents;
pub mod bash;
pub mod bash_jobs;
mod canonical;
pub mod files;
pub mod fs_walk;
pub mod image;
pub mod inbox;
pub mod jobs;
pub mod net_guard;
pub mod read;
pub mod search;
pub mod skill;
mod slug;
pub mod tasks;
#[cfg(test)]
mod testkit;
pub mod tool_search;
pub mod turn_gate;
pub mod web_fetch;

pub use agents::{AgentDirectory, SubagentHost, SubagentHub};
pub use bash::{BashOptions, BashTool};
pub use bash_jobs::job_tools;
pub use files::{EditTool, WriteTool};
pub use image::{ImageTool, ImageToolOptions};
pub use inbox::Inbox;
pub use jobs::{JobEvents, JobHub};
pub use read::{ReadIntercept, ReadTool};
pub use search::{GlobTool, GrepTool};
pub use skill::{SkillToolOptions, create_skill_tool};
pub use tasks::create_task_tools;
pub use turn_gate::AgentTurnGate;
pub use web_fetch::{WebFetchNet, WebFetchTool};

/// What a host can change about the built-in tools. An option left None has
/// no tool: [`builtin_tools`] leaves it out, and [`builtin_tools_where`]
/// refuses a pick that turns its word on (a tool is never silently missing).
#[derive(Clone)]
pub struct BuiltinOptions {
    /// PLUG POINT: asked before the workspace fs on every Read, so a host can
    /// answer some paths itself (an `attachment:<id>` that is no path at all).
    pub read_intercept: Option<Arc<dyn ReadIntercept>>,
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

/// The built-in tools, in the order the model sees them. With a job hub
/// ([`BashOptions::jobs`]) BashOutput and KillShell follow Bash.
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
    tools.extend(opts.tasks.map(create_task_tools).unwrap_or_default());
    tools.extend(opts.skill.map(create_skill_tool));
    tools
}

/// The word that picks a built-in tool (the `tools` setting): `read`, `write`,
/// `edit`, `bash` (with its job tools), `grep`, `glob`, `web_fetch`, `tasks`,
/// `skill`.
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

/// The built-in tools whose word `on` takes, in [`builtin_tools`]' order. A
/// word that is on while the option its tool needs is None is an error: a
/// tool must never be silently missing.
pub fn builtin_tools_where(
    on: impl Fn(&str) -> bool,
    opts: BuiltinOptions,
) -> Result<Vec<Arc<dyn Tool>>, String> {
    let needs = [
        ("web_fetch", opts.web_fetch.is_some()),
        ("tasks", opts.tasks.is_some()),
        ("skill", opts.skill.is_some()),
    ];
    if let Some((word, _)) = needs.iter().find(|(word, given)| !given && on(word)) {
        return Err(format!(
            "the tools setting has {word} on, but BuiltinOptions.{word} is None"
        ));
    }
    Ok(builtin_tools(opts)
        .into_iter()
        .filter(|t| builtin_word(t.name()).is_some_and(&on))
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(tools: &[Arc<dyn Tool>]) -> Vec<String> {
        tools.iter().map(|t| t.name().to_string()).collect()
    }

    #[test]
    fn the_builtin_list_has_every_tool_with_its_kind_and_web_fetch_is_optional() {
        let all = builtin_tools(BuiltinOptions::default());
        assert_eq!(
            names(&all),
            ["Read", "Write", "Edit", "Bash", "Grep", "Glob", "WebFetch"]
        );
        let kinds: Vec<_> = all.iter().map(|t| t.kind()).collect();
        assert_eq!(
            kinds,
            [
                "file.read",
                "file.write",
                "file.edit",
                "shell.exec",
                "text.search",
                "file.glob",
                "web.fetch"
            ]
        );
        let read_only: Vec<_> = all
            .iter()
            .filter(|t| t.read_only())
            .map(|t| t.name())
            .collect();
        assert_eq!(read_only, ["Read", "Grep", "Glob", "WebFetch"]);
        let offline = builtin_tools(BuiltinOptions {
            web_fetch: None,
            ..Default::default()
        });
        assert!(!names(&offline).contains(&"WebFetch".to_string()));
    }

    /// haest-core's prompt and Code go by FILE_TOOLS: a renamed tool here
    /// would drop its path line silently.
    #[test]
    fn every_file_tool_haest_core_names_is_a_builtin() {
        let all = names(&builtin_tools(BuiltinOptions::default()));
        for name in haest_core::tool::FILE_TOOLS {
            assert!(all.iter().any(|t| t == name), "{name} is not a built-in");
        }
    }

    #[tokio::test]
    async fn a_job_hub_adds_the_job_tools_after_bash() {
        let hub = Arc::new(JobHub::new(Arc::new(inbox::NoInbox)));
        let tools = builtin_tools(BuiltinOptions {
            bash: BashOptions {
                jobs: Some(hub),
                ..Default::default()
            },
            web_fetch: None,
            ..Default::default()
        });
        assert_eq!(
            names(&tools),
            [
                "Read",
                "Write",
                "Edit",
                "Bash",
                "BashOutput",
                "KillShell",
                "Grep",
                "Glob"
            ]
        );
        let bash = &tools[3];
        assert!(bash.parameters()["properties"]["run_in_background"].is_object());
    }

    #[tokio::test]
    async fn a_host_picks_built_ins_by_word_and_bash_brings_its_job_tools() {
        let pick = |words: &'static [&str], jobs| {
            let opts = BuiltinOptions {
                bash: BashOptions {
                    jobs,
                    ..Default::default()
                },
                ..Default::default()
            };
            names(&builtin_tools_where(|w| words.contains(&w), opts).unwrap())
        };
        assert!(pick(&[], None).is_empty());
        assert_eq!(pick(&["web_fetch", "read"], None), ["Read", "WebFetch"]);
        let hub = Arc::new(JobHub::new(Arc::new(inbox::NoInbox)));
        assert_eq!(
            pick(&["bash"], Some(hub)),
            ["Bash", "BashOutput", "KillShell"]
        );
        let every = builtin_tools(BuiltinOptions::default());
        assert!(every.iter().all(|t| builtin_word(t.name()).is_some()));
    }

    #[test]
    fn a_word_that_is_on_is_never_silently_dropped() {
        let skills = || Vec::new();
        let fs = Arc::new(haest_core::fs::MemFs::new());
        let given = BuiltinOptions {
            tasks: Some(Arc::new(TaskBoard::default())),
            skill: Some(SkillToolOptions::new(skills, fs)),
            ..Default::default()
        };
        let pick = |word: &str, opts: BuiltinOptions| builtin_tools_where(|w| w == word, opts);
        let tasks = pick("tasks", given.clone()).unwrap();
        assert_eq!(
            names(&tasks),
            ["TaskCreate", "TaskUpdate", "TaskList", "TaskGet"]
        );
        assert_eq!(names(&pick("skill", given).unwrap()), ["Skill"]);
        for word in ["web_fetch", "tasks", "skill"] {
            let none = BuiltinOptions {
                web_fetch: None,
                ..Default::default()
            };
            let err = pick(word, none).err().unwrap();
            assert!(err.contains(word), "{err}");
        }
    }
}
