//! kern's built-in tools. PORT (T7): `tools/*.ts`.
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

use kern_core::tool::Tool;

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
pub use turn_gate::AgentTurnGate;
pub use web_fetch::{WebFetchNet, WebFetchTool};

/// What a host can change about the built-in tools.
#[derive(Clone)]
pub struct BuiltinOptions {
    /// PLUG POINT: asked before the workspace fs on every Read, so a host can
    /// answer some paths itself (an `attachment:<id>` that is no path at all).
    pub read_intercept: Option<Arc<dyn ReadIntercept>>,
    pub bash: BashOptions,
    /// WebFetch's resolver and HTTP client; None = no WebFetch tool at all (a
    /// host with its own fetch, or no door to the network).
    pub web_fetch: Option<WebFetchNet>,
}

impl Default for BuiltinOptions {
    fn default() -> Self {
        Self {
            read_intercept: None,
            bash: BashOptions::default(),
            web_fetch: Some(WebFetchNet::default()),
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
    tools
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(tools: &[Arc<dyn Tool>]) -> Vec<String> {
        tools.iter().map(|t| t.name().to_string()).collect()
    }

    #[test]
    fn the_builtin_list_has_every_tool_and_web_fetch_is_optional() {
        let all = builtin_tools(BuiltinOptions::default());
        assert_eq!(
            names(&all),
            ["Read", "Write", "Edit", "Bash", "Grep", "Glob", "WebFetch"]
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

    #[test]
    fn every_tool_states_its_canonical_kind() {
        let kinds: Vec<_> = builtin_tools(BuiltinOptions::default())
            .iter()
            .map(|t| t.kind().to_string())
            .collect();
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
    }
}
