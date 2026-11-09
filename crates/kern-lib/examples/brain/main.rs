//! kern embedded the way brain embeds it, against fakes: no network, no keys.
//! Every row of PLAN.md's "Brain's plug points" table is wired through a
//! public plug point and ASSERTED, so `cargo run -p kern-lib --example brain`
//! exiting 0 is the proof.
//!
//! | Plug point | Where |
//! |---|---|
//! | `Transport` (the node's gateway); OpenRouter routing off | gateway.rs, `models` |
//! | `Fs` (BrainFs, writes staged) under `SandboxWorkspace::over` | brain_fs.rs, `runs::work` |
//! | `ShutWorkspace` (the dream run) | `runs::dream` |
//! | Read with a `ReadIntercept` (`attachment:<id>`) | tools.rs |
//! | Bash without background jobs | `work_tools` |
//! | a host tool (`brain_search`, `dream_material`) | tools.rs |
//! | `AgentHooks::now_ms` (a pause does not count) | host.rs, `runs::work` + `runs::clock` |
//! | `LoopEvent::ToolCallStart` (narration taken out of the doc) | doc.rs |
//! | `SessionSink` + `replay`, no JSONL writer of kern's | host.rs, `runs::work` |
//! | Agent tools over the host's own `AgentDirectory` | tools.rs |

mod brain_fs;
mod doc;
mod gateway;
mod host;
mod runs;
mod tools;

use std::sync::Arc;

use kern::{
    AgentDeps, ModelResolver, ResolvedModel, Workspace,
    provider::parse_model_spec,
    providers::{CompatFlavor, CompatOptions, CompatProvider, RetryPolicy},
    tool::Tool,
    tools::{
        BashOptions, BashTool, EditTool, ReadTool, WriteTool,
        agents::{AgentRef, agent_tools},
    },
};

use crate::{
    gateway::{BASE_URL, Gateway},
    host::BrainHost,
    tools::{Attachments, BrainSearch, NodeAgents},
};

pub const MODEL: &str = "gateway:anthropic/claude-sonnet-5";
pub const CWD: &str = "/brain";
/// The run clock's start, milliseconds since the epoch.
pub const T0: u64 = 1_800_000_000_000;

#[tokio::main]
async fn main() {
    let gateway = Arc::new(Gateway::default());
    runs::work(&gateway).await;
    runs::dream(&gateway).await;
    runs::clock(&gateway).await;
    println!("brain example: every plug point held");
}

/// `gateway:<model>` -> the OpenAI-compatible adapter over the node's gateway.
/// The gateway speaks OpenRouter's dialect, but decides routing itself: no
/// `provider` object goes out (`routing: None`, the default).
pub fn models(gateway: Arc<Gateway>) -> Arc<dyn ModelResolver> {
    // The run holds no keys: the gateway adds them.
    let mut opts = CompatOptions::new("gateway", CompatFlavor::OpenRouter, "");
    opts.base_url = Some(BASE_URL.into());
    let provider = Arc::new(CompatProvider::new(opts, gateway).with_retry(RetryPolicy::none()));
    Arc::new(move |spec: &str| {
        let (id, model) = parse_model_spec(spec)?;
        if id != "gateway" {
            return Err(format!(
                "brain runs every model through its gateway, not \"{id}\""
            ));
        }
        Ok(ResolvedModel {
            provider: provider.clone(),
            model: model.into(),
        })
    })
}

/// brain's tools for a work run: kern's Read (with the attachment intercept),
/// Write, Edit and Bash (no background jobs), brain's own search, and the agent
/// tools over the node's hub. Glob and Grep are brain's own on the node.
pub fn work_tools(attachments: Arc<Attachments>, agents: Arc<NodeAgents>) -> Vec<Arc<dyn Tool>> {
    let mut tools: Vec<Arc<dyn Tool>> = vec![
        Arc::new(ReadTool::new(Some(attachments))),
        Arc::new(WriteTool),
        Arc::new(EditTool),
        Arc::new(BashTool::new(BashOptions {
            jobs: None,
            ..Default::default()
        })),
        Arc::new(brain_search()),
    ];
    tools.extend(agent_tools(agents, AgentRef::main(), true));
    tools
}

pub fn brain_search() -> BrainSearch {
    BrainSearch {
        notes: vec![
            ("/brain/notes/plan.md", "Plan", "Ship the Q3 review."),
            ("/brain/notes/q2.md", "Q2", "Revenue flat in Q2."),
        ],
    }
}

/// The deps every run shares; the workspace, tools and host differ per run.
pub fn deps(
    gateway: &Arc<Gateway>,
    tools: Vec<Arc<dyn Tool>>,
    workspace: Arc<dyn Workspace>,
    host: Arc<BrainHost>,
) -> AgentDeps {
    let system = Arc::new("You work inside a person's brain, a shared knowledge base.".to_string());
    let mut deps = AgentDeps::new(
        models(gateway.clone()),
        Arc::new(tools),
        system,
        CWD,
        workspace,
    );
    deps.hooks = host;
    deps
}
