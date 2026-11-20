//! haest embedded in a notebook: a shared knowledge base whose node runs
//! the agent and brings its own parts. Against fakes: no network, no keys.
//! Every row of PLAN.md's plug-point table (from a real host's patches) is
//! wired through a public plug point and ASSERTED, so
//! `cargo run -p haest --example notebook` exiting 0 is the proof; `cargo
//! test` runs the same (`[[example]] test`).
//! A host that takes haest's own subagent hub, shell commands and steering
//! instead: `tests/plug_in.rs`.
//!
//! | Plug point | Where |
//! |---|---|
//! | `settings::Settings` written out, one per kind of run (no loader) | config.rs |
//! | `builtin_tools_where`: the built-ins the settings pick | `run_tools` |
//! | `http::HttpClient` (the node's gateway); OpenRouter routing off | gateway.rs, `models` |
//! | `Fs` (NotebookFs, writes staged) under `SandboxWorkspace::over` | notebook_fs.rs, `runs::work` |
//! | `ShutWorkspace` (the dream run) | `runs::dream` |
//! | Read with a `ReadIntercept` (`attachment:<id>`) | tools.rs |
//! | Bash without background jobs | `run_tools` |
//! | a host tool (`notebook_search`, `dream_material`) | tools.rs |
//! | `Clock` (a pause does not count), `TurnCapsSource`, in one host value (`SharedDeps::with_host`) | host.rs, `shared`, `runs::work` + `runs::clock` |
//! | `LoopEvent::ToolCallStart` (narration taken out of the doc) | doc.rs |
//! | `SessionSink` + `replay`, no JSONL writer of haest's | host.rs, `runs::work` |
//! | haest's `SubagentHub`, each helper on a node run of its own (`SubagentParent::start_member`, `MemberRun`); the notebook's kinds and model text (`AgentToolOptions`) | helpers.rs, `runs::work` |
//! | the person writing to a running helper (`SubagentHub::deliver` from `Sender::Person`), the panel's messages (`SubagentEvent::Message`), answers held for main (`Inbox::submit_answer`), waiting for the helpers (`SubagentHub::settled`), the notebook's own note (`SubagentParent::subagent_note`) | helpers.rs, panel.rs |

mod config;
mod doc;
mod gateway;
mod helpers;
mod host;
mod notebook_fs;
mod panel;
mod runs;
mod tools;

use haest::{
    AgentDeps, BoxError, ModelError, ModelResolverRef, ProviderRef, ResolvedModel, SharedDeps,
    Tools, WorkspaceRef,
    core::provider::parse_model_spec,
    providers::{CompatFlavor, CompatOptions, CompatProvider, RetryPolicy},
    settings::Settings,
    tools::{AgentToolOptions, BuiltinOptions, SubagentTypes, builtin_tools_where},
};

use crate::{
    gateway::{BASE_URL, Gateway},
    host::NotebookHost,
    tools::{Attachments, NotebookSearch},
};

/// The model every run of the notebook uses, through its gateway.
pub(crate) const MODEL: &str = "gateway:anthropic/claude-sonnet-5";
/// Where the notebook's files are, as its runs see them.
pub(crate) const CWD: &str = "/notebook";
/// The run clock's start, milliseconds since the epoch.
pub(crate) const T0: u64 = 1_800_000_000_000;

#[tokio::main]
#[expect(
    clippy::print_stdout,
    reason = "an example shows on the terminal what it does"
)]
async fn main() -> Result<(), BoxError> {
    every_run().await?;
    println!("notebook example: every plug point held");
    Ok(())
}

async fn every_run() -> Result<(), BoxError> {
    let gateway = Gateway::default();
    runs::work(&gateway).await?;
    runs::dream(&gateway).await?;
    runs::clock(&gateway).await;
    panel::panel(&gateway).await
}

#[tokio::test]
async fn every_plug_point_the_notebook_uses_holds() -> Result<(), BoxError> {
    every_run().await
}

/// `gateway:<model>` -> the OpenAI-compatible adapter over the node's gateway.
/// The gateway speaks OpenRouter's dialect, but decides routing itself: no
/// `provider` object goes out (`routing: None`, the default). `token` is the
/// node run's own (a helper's), or empty.
pub(crate) fn models(gateway: Gateway, token: &str) -> ModelResolverRef {
    // The run holds no provider keys: the gateway adds them.
    let opts = CompatOptions::new("gateway", token)
        .with_flavor(CompatFlavor::OpenRouter)
        .with_base_url(BASE_URL);
    let provider =
        ProviderRef::new(CompatProvider::new(opts, gateway).with_retry(RetryPolicy::none()));
    ModelResolverRef::new(move |spec: &str| {
        let (id, model) = parse_model_spec(spec)?;
        if id != "gateway" {
            // The notebook runs every model through its gateway.
            return Err(ModelError::UnknownProvider {
                provider: id.into(),
                known: vec!["gateway".into()],
            });
        }
        Ok(ResolvedModel::new(provider.clone(), model))
    })
}

/// A run's tools as its settings pick them: haest's built-ins (`tools`; Read
/// with the attachment intercept, Bash with no background jobs, no WebFetch:
/// the node fetches) and the notebook's own (`own`). The agent tools come from
/// the hub, over these (`runs::work`).
pub(crate) fn run_tools(
    s: &Settings,
    own: Tools,
    attachments: Attachments,
) -> Result<Tools, BoxError> {
    let opts = BuiltinOptions::new()
        .with_read_intercept(attachments)
        .without_web_fetch();
    // A word on whose option is not given here (web_fetch, tasks, skill) is an
    // error, never a tool quietly missing; the notebook's runs turn on none of them.
    let mut tools = builtin_tools_where(|t| s.tools.has(t.as_str()), opts)?;
    tools.extend(own);
    Ok(tools)
}

/// The notebook's kinds of subagent (`explore` only reads) and the models it offers.
pub(crate) fn agent_options() -> AgentToolOptions {
    // The hub offers a budget from its own caps (subagent_budgets); the notebook's are off.
    AgentToolOptions::default()
        .with_types(
            SubagentTypes::new("explore")
                .with_kinds(["explore", "general"])
                .with_description(
                    "explore: only reads and searches (the cheap, safe choice for finding things). general: your tools, file changes included (staged like yours). Default explore.",
                ),
        )
        .with_model_description(format!(
            "The model to run it on (optional; defaults to yours): {MODEL}"
        ))
}

/// The notebook's search tool over two notes.
pub(crate) fn notebook_search() -> NotebookSearch {
    NotebookSearch {
        notes: vec![
            ("/notebook/notes/plan.md", "Plan", "Ship the Q3 review."),
            ("/notebook/notes/q2.md", "Q2", "Revenue flat in Q2."),
        ],
    }
}

/// What a run shares with its helpers; the node run's token, the workspace
/// and host differ per run. The host is one value in every host slot.
pub(crate) fn shared(
    gateway: &Gateway,
    token: &str,
    workspace: impl Into<WorkspaceRef>,
    host: NotebookHost,
) -> SharedDeps {
    SharedDeps::new(models(gateway.clone(), token), CWD, workspace).with_host(host)
}

/// A run's deps: what it shares ([`shared`]), its tools and the notebook's
/// system prompt.
pub(crate) fn deps(
    gateway: &Gateway,
    token: &str,
    tools: Tools,
    workspace: impl Into<WorkspaceRef>,
    host: NotebookHost,
) -> AgentDeps {
    let system = "You work inside a person's notebook, a shared knowledge base.";
    AgentDeps::new(shared(gateway, token, workspace, host), tools, system)
}
