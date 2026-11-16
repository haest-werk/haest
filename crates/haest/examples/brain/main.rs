//! haest embedded the way brain embeds it, against fakes: no network, no keys.
//! Every row of PLAN.md's "Brain's plug points" table is wired through a
//! public plug point and ASSERTED, so `cargo run -p haest --example brain`
//! exiting 0 is the proof; `cargo test` runs the same (`[[example]] test`).
//! A host that takes haest's own sub-agent hub, shell commands and steering
//! instead: `tests/plug_in.rs`.
//!
//! | Plug point | Where |
//! |---|---|
//! | `settings::Settings` written out, one per kind of run (no loader) | config.rs |
//! | `builtin_tools_where`: the built-ins the settings pick | `run_tools` |
//! | `http::HttpClient` (the node's gateway); OpenRouter routing off | gateway.rs, `models` |
//! | `Fs` (BrainFs, writes staged) under `SandboxWorkspace::over` | brain_fs.rs, `runs::work` |
//! | `ShutWorkspace` (the dream run) | `runs::dream` |
//! | Read with a `ReadIntercept` (`attachment:<id>`) | tools.rs |
//! | Bash without background jobs | `run_tools` |
//! | a host tool (`brain_search`, `dream_material`) | tools.rs |
//! | `Clock` (a pause does not count), `ModelFacts::turn_caps` | host.rs, `runs::work` + `runs::clock` |
//! | `LoopEvent::ToolCallStart` (narration taken out of the doc) | doc.rs |
//! | `SessionSink` + `replay`, no JSONL writer of haest's | host.rs, `runs::work` |
//! | Agent tools over the host's own `AgentDirectory` | tools.rs |

mod brain_fs;
mod config;
mod doc;
mod gateway;
mod host;
mod runs;
mod tools;

use std::sync::Arc;

use haest::{
    AgentDeps, ModelError, ModelResolver, ResolvedModel, Workspace,
    core::provider::parse_model_spec,
    core::tool::Tool,
    providers::{CompatFlavor, CompatOptions, CompatProvider, RetryPolicy},
    settings::{Settings, WordSet},
    tools::{
        BashOptions, BuiltinOptions,
        agents::{AgentRef, AgentToolOptions, SubagentTypes, agent_tools_with},
        builtin_tools_where,
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
    every_run().await;
    println!("brain example: every plug point held");
}

async fn every_run() {
    let gateway = Arc::new(Gateway::default());
    runs::work(&gateway).await;
    runs::dream(&gateway).await;
    runs::clock(&gateway).await;
}

#[tokio::test]
async fn every_plug_point_brain_uses_holds() {
    every_run().await;
}

/// `gateway:<model>` -> the OpenAI-compatible adapter over the node's gateway.
/// The gateway speaks OpenRouter's dialect, but decides routing itself: no
/// `provider` object goes out (`routing: None`, the default).
pub fn models(gateway: Arc<Gateway>) -> Arc<dyn ModelResolver> {
    // The run holds no keys: the gateway adds them.
    let mut opts = CompatOptions::new("gateway", "");
    opts.flavor = CompatFlavor::OpenRouter;
    opts.base_url = Some(BASE_URL.into());
    let provider = Arc::new(CompatProvider::new(opts, gateway).with_retry(RetryPolicy::none()));
    Arc::new(move |spec: &str| {
        let (id, model) = parse_model_spec(spec)?;
        if id != "gateway" {
            // brain runs every model through its gateway.
            return Err(ModelError::UnknownProvider {
                provider: id.into(),
                known: vec!["gateway".into()],
            });
        }
        Ok(ResolvedModel {
            provider: provider.clone(),
            model: model.into(),
        })
    })
}

/// A run's tools as its settings pick them: haest's built-ins (`tools`; Read
/// with the attachment intercept, Bash with no background jobs, no WebFetch:
/// the node fetches), brain's own (`own`), and the agent tools over the node's
/// hub (`agents`).
pub fn run_tools(
    s: &Settings,
    own: Vec<Arc<dyn Tool>>,
    attachments: Arc<Attachments>,
    agents: Arc<NodeAgents>,
) -> Vec<Arc<dyn Tool>> {
    let opts = BuiltinOptions {
        read_intercept: Some(attachments),
        bash: BashOptions {
            jobs: None,
            ..Default::default()
        },
        web_fetch: None,
        ..Default::default()
    };
    // A word on whose option is None here (web_fetch, tasks, skill) would be
    // an error, never a tool quietly missing; brain's runs turn on none of them.
    let mut tools = builtin_tools_where(|w| s.tools.has(w), opts)
        .expect("brain's runs pick only built-ins it gives the options for");
    tools.extend(own);
    if s.agents {
        tools.extend(agent_tools_with(
            agents,
            AgentRef::main(),
            true,
            agent_options(),
        ));
    }
    tools
}

/// brain's kinds of sub-agent (`explore` only reads) and the models it offers.
pub fn agent_options() -> AgentToolOptions {
    AgentToolOptions {
        types: Some(SubagentTypes {
            names: vec!["explore".into(), "general".into()],
            description: "explore: only reads and searches (the cheap, safe choice for finding things). general: your tools, file changes included (staged like yours). Default explore.".into(),
            default: "explore".into(),
        }),
        models: Some(format!("The model to run it on (optional; defaults to yours): {MODEL}")),
    }
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
    deps.hooks = host.clone();
    deps.facts = host.clone();
    deps.clock = host;
    deps
}
