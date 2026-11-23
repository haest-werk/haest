//! Hooks in another process, against a scripted model: no keys, and no
//! network past 127.0.0.1. `cargo run -p haest --example external_hooks
//! --features bridge-shell,bridge-http`; `cargo test` runs it too.
//!
//! A coding agent whose host binds two hooks it did not write in Rust:
//!
//! - `docs/examples/hooks/no-rm-rf.sh`, a Claude Code `PreToolUse` script,
//!   run as it is (`ShellHook` in the Claude dialect), guards Bash;
//! - a reviewer service on `turn.end` (`HttpHook`, haest's dialect) sends
//!   the turn back once with "run the tests": the turn's `continued` tells it
//!   whether it already did. haest-http's test server (`testing::Server`)
//!   stands in for the service.
//!
//! The model tries `rm -rf build` (refused), says it is done (sent back),
//! runs the tests and is done.

#![expect(
    clippy::print_stdout,
    reason = "an example shows on the terminal what it does"
)]

use std::{
    path::Path,
    sync::{Arc, Mutex},
};

use haest::{
    Agent, AgentDeps, AgentOptions, BoxError, JsonMap, LoopEvent, Message, NoHooks, SharedDeps,
    ShutWorkspace, Tool, ToolContext, ToolError, ToolOutput, Tools,
    bridges::{Claude, HttpHook, ShellHook},
    core::{
        testing::{ScriptedProvider, text_answer, tool_answer},
        workspace::WorkspaceMode,
    },
    http::testing::{Respond, Server},
    mods::{
        ApprovalRequest, Binding, Decision, Hooks, ModEnv, ModLoopHooks, ModTools, ModUi, Mods,
        SessionView,
    },
    serde_json::{Value, json},
};

/// The Claude Code hook script, from the repository's docs.
const NO_RM_RF_SH: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../docs/examples/hooks/no-rm-rf.sh"
);

/// A stand-in for the shell: it says what it would have run, and keeps a list.
#[derive(Default)]
struct Bash {
    ran: Mutex<Vec<String>>,
}

#[haest::async_trait]
impl Tool for Bash {
    fn name(&self) -> &str {
        "Bash"
    }
    fn description(&self) -> String {
        "Run a shell command.".into()
    }
    fn parameters(&self) -> Value {
        json!({"type": "object", "properties": {"command": {"type": "string"}}})
    }
    fn kind(&self) -> &str {
        "shell.exec"
    }
    async fn execute(&self, input: JsonMap, _: &ToolContext) -> Result<ToolOutput, ToolError> {
        let command = input.get("command").and_then(Value::as_str).unwrap_or("");
        let mut ran = self.ran.lock().expect("the list's lock poisoned");
        ran.push(command.to_string());
        Ok(ToolOutput::text(format!("ran: {command}")))
    }
}

/// The host's terminal: what the mods tell the human.
struct Terminal;

#[haest::async_trait]
impl ModUi for Terminal {
    async fn ask(&self, _: ApprovalRequest, _: &haest::CancellationToken) -> Decision {
        Decision::Deny
    }
    fn info(&self, text: &str) {
        println!("(for the human) {text}");
    }
}

/// The reviewer service, on a free port of 127.0.0.1: each finished turn is
/// POSTed here as haest's `turn.end` JSON. A turn nobody sent back yet goes
/// back once (an empty answer lets a turn end).
fn reviewer() -> Server {
    Server::answering(|post| {
        let turn: Value = serde_json::from_slice(&post.body).unwrap_or_default();
        let decision = if turn["continued"] == true {
            String::new()
        } else {
            json!({ "continue": "Run the tests before you stop." }).to_string()
        };
        Respond::With(200, decision)
    })
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), BoxError> {
    run().await
}

#[tokio::test]
async fn a_script_refuses_rm_rf_and_a_server_sends_the_turn_back_once() -> Result<(), BoxError> {
    run().await
}

async fn run() -> Result<(), BoxError> {
    let project = Path::new(env!("CARGO_MANIFEST_DIR"));

    // A Claude Code hook script as it is, on Bash calls only.
    let guard = ShellHook::new(format!("sh '{NO_RM_RF_SH}'"))
        .with_cwd(project)
        .with_dialect(Claude);
    // A service on every finished turn.
    let reviewer = reviewer();
    let review = HttpHook::new(reviewer.url("/turn-end"));
    let hooks = Hooks::new()
        .on_tool_before(Binding::new(guard).with_matcher("Bash"))
        .on_turn_end(review);
    // The session the conversation is on: each hook is told it at every
    // event (the script as `session_id`, the reviewer as `session`), and a
    // /clear names the new one with `mods.env().set_session(..)`.
    let env = ModEnv::new(project, WorkspaceMode::Real, Terminal)
        .with_session(SessionView::new("example"));
    let mods = Mods::new(env, [hooks]);

    let model = ScriptedProvider::new([
        tool_answer(&[("a", "Bash", json!({"command": "rm -rf build"}))]),
        text_answer("Done."),
        tool_answer(&[("b", "Bash", json!({"command": "cargo test"}))]),
        text_answer("The tests pass."),
    ]);
    let bash = Arc::new(Bash::default());
    let tools = Tools::new().with(bash.clone());
    let shared = SharedDeps::new(model.resolver(), project, ShutWorkspace);
    let deps = AgentDeps::new(shared, ModTools::new(tools, mods.clone()), "You fix code.")
        .with_hooks(ModLoopHooks::new(NoHooks, mods));
    let mut agent = Agent::new(deps, AgentOptions::new("scripted:coder"));

    let mut sink = |ev: LoopEvent| {
        if let LoopEvent::TextDelta { text } = ev {
            println!("model: {text}");
        }
    };
    let end = agent.run("The build is broken, fix it.", &mut sink).await;
    assert!(end.error.is_none(), "{end:?}");

    // The script refused `rm -rf`, with its reason for the model.
    let refused = agent.history().iter().any(
        |m| matches!(m, Message::Tool(r) if r.is_error && r.content.contains("rm -rf is not allowed here")),
    );
    assert!(refused, "the guard refused rm -rf");
    // The reviewer sent the turn back once; then the tests ran.
    let sent_back = agent
        .history()
        .iter()
        .filter(|m| matches!(m, Message::User(u) if u.content == "Run the tests before you stop."))
        .count();
    assert_eq!(sent_back, 1);
    let ran = bash.ran.lock().expect("the list's lock poisoned").clone();
    assert_eq!(ran, ["cargo test"]);
    println!("Bash ran {ran:?}; rm -rf was refused, the turn went back once.");
    Ok(())
}
