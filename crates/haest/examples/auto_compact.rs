//! Compact as a turn ends, once the context is past 60% of the window, in
//! both forms, against a scripted model: no keys, no network. `cargo run -p
//! haest --example auto_compact`; `cargo test` runs it too.
//!
//! The same policy twice, and the same conversation for each:
//!
//! - a Rust hook (`Hooks::on_turn_end`), the form a host like brain uses;
//! - the mod file `docs/examples/mods/auto-compact.ts`, loaded the way a CLI
//!   loads a user's mod files (`ModFiles`, from its `mods/` directory).
//!
//! A long conversation resumes at half the window. The first turn's answer
//! takes it to 65%: the hook asks for a compaction, with what the summary
//! must keep, and the next turn starts from the summary. The second turn
//! stays small and nothing compacts.

#![expect(
    clippy::print_stdout,
    reason = "an example shows on the terminal what it does"
)]

use haest::{
    Agent, AgentDeps, AgentOptions, BoxError, CancellationToken, LoopEvent, Message, NoHooks,
    SharedDeps, ShutWorkspace, Tools, TurnCaps,
    bridges::{ModFileOptions, ModFiles},
    core::{
        MemFs,
        testing::{ScriptedProvider, text_answer},
        workspace::WorkspaceMode,
    },
    mods::{
        ApprovalRequest, Compact, Decision, Hooks, ModEnv, ModLoopHooks, ModTools, ModUi, Mods,
        TurnEndDecision, TurnEndPayload,
    },
};

/// The mod file, from the repository's docs.
const AUTO_COMPACT_TS: &str = include_str!("../../../docs/examples/mods/auto-compact.ts");

/// The model's window, as the catalog would give it.
const WINDOW: u64 = 200_000;
/// The share past which a finished turn compacts (the mod file's `SHARE`).
const SHARE: f64 = 0.6;
/// What the summary must keep (the mod file's `KEEP`).
const KEEP: &str = "Keep the open tasks and every file path.";

/// The Rust form: the mod file's policy as a closure.
fn rust_hook() -> Hooks {
    Hooks::new().on_turn_end(|turn: TurnEndPayload, _: CancellationToken| async move {
        let view = turn.agent;
        let full = view
            .context_window
            .is_some_and(|window| view.context_tokens as f64 >= window as f64 * SHARE);
        let decision = TurnEndDecision::default();
        Ok::<_, BoxError>(if full {
            decision.with_compact(Compact::new().with_instructions(KEEP))
        } else {
            decision
        })
    })
}

/// The TypeScript form: the mod file, in the user's mod directory.
async fn ts_mod(env: ModEnv) -> ModFiles {
    let disk = MemFs::from_files([(
        "/home/me/.config/haest/mods/auto-compact.ts",
        AUTO_COMPACT_TS,
    )]);
    let opts = ModFileOptions::new("/home/me/.config/haest").with_user_mods(true);
    let files = ModFiles::load(opts, disk, env).await;
    assert_eq!(
        files.names(),
        ["/home/me/.config/haest/mods/auto-compact.ts"]
    );
    files
}

/// The host's terminal: nothing here asks the human.
struct Terminal;

#[haest::async_trait]
impl ModUi for Terminal {
    async fn ask(&self, _: ApprovalRequest, _: &CancellationToken) -> Decision {
        Decision::Deny
    }
    fn info(&self, text: &str) {
        println!("(for the human) {text}");
    }
}

/// An answer whose request held `tokens` of context.
fn answer(text: &str, tokens: u64) -> Message {
    let mut a = text_answer(text);
    a.usage.input = 1_000;
    a.usage.cache_read = tokens - 1_000;
    Message::Assistant(a)
}

/// A long conversation about a refactor, its last request at half the window.
fn long_conversation() -> Vec<Message> {
    let mut history = Vec::new();
    for (i, file) in [
        "src/a.rs", "src/b.rs", "src/c.rs", "src/d.rs", "src/e.rs", "src/f.rs", "src/g.rs",
    ]
    .into_iter()
    .enumerate()
    {
        history.push(Message::user(format!("Refactor {file}.")));
        let tokens = (i as u64 + 1) * WINDOW / 14;
        history.push(answer(&format!("{file} done."), tokens));
    }
    history
}

/// The conversation, with `mods` on: what each request was, and the history.
async fn conversation(
    mods: Mods,
) -> Result<(Vec<haest::core::ProviderRequest>, Vec<Message>), BoxError> {
    let mut full = text_answer("src/h.rs done; src/i.rs is next.");
    full.usage.input = 1_000;
    full.usage.cache_read = WINDOW * 65 / 100 - 1_000;
    let model = ScriptedProvider::new([
        full,
        text_answer("Refactored src/a.rs to src/h.rs; open: src/i.rs."),
        text_answer("src/i.rs done."),
    ]);
    let caps = TurnCaps {
        context_window: WINDOW,
        ..TurnCaps::default()
    };
    let shared = SharedDeps::new(model.resolver(), "/project", ShutWorkspace).with_turn_caps(caps);
    let deps = AgentDeps::new(
        shared,
        ModTools::new(Tools::new(), mods.clone()),
        "You refactor code.",
    )
    .with_hooks(ModLoopHooks::new(NoHooks, mods));
    let opts = AgentOptions::new("scripted:coder").with_history(long_conversation());
    let mut agent = Agent::new(deps, opts);

    let mut sink = |ev: LoopEvent| {
        if let LoopEvent::TextDelta { text } = ev {
            println!("model: {text}");
        }
    };
    for input in ["Refactor src/h.rs.", "Refactor src/i.rs."] {
        let end = agent.run(input, &mut sink).await;
        assert!(end.error.is_none(), "{end:?}");
    }
    Ok((model.requests(), agent.history().to_vec()))
}

/// The conversation compacted once, after the first turn, keeping `KEEP`.
fn compacted_once(requests: &[haest::core::ProviderRequest], history: &[Message]) {
    // The turn, the summary, the next turn: three requests.
    assert_eq!(requests.len(), 3);
    let asked = requests[1].messages.last();
    let keeps = matches!(asked, Some(Message::User(u)) if u.content.contains(KEEP));
    assert!(keeps, "the summary request asks to keep {KEEP:?}");
    // The next turn starts from the summary: the oldest messages are gone.
    let summary = "Refactored src/a.rs to src/h.rs; open: src/i.rs.";
    let from_summary =
        |m: &[Message]| matches!(m.first(), Some(Message::User(u)) if u.content.contains(summary));
    assert!(from_summary(&requests[2].messages));
    assert!(from_summary(history));
    let oldest = |m: &Message| matches!(m, Message::User(u) if u.content == "Refactor src/a.rs.");
    assert!(requests[0].messages.iter().any(oldest));
    assert!(!requests[2].messages.iter().any(oldest));
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), BoxError> {
    run().await
}

#[tokio::test]
async fn a_rust_hook_and_a_mod_file_compact_past_sixty_percent() -> Result<(), BoxError> {
    run().await
}

async fn run() -> Result<(), BoxError> {
    let env = || ModEnv::new("/project", WorkspaceMode::Real, Terminal);

    let (requests, history) = conversation(Mods::new(env(), [rust_hook()])).await?;
    compacted_once(&requests, &history);
    println!(
        "The Rust hook compacted at 65% of the window; {} messages now.",
        history.len()
    );

    let files = ts_mod(env()).await;
    let (requests, history) = conversation(Mods::new(env(), [files])).await?;
    compacted_once(&requests, &history);
    println!(
        "The mod file compacted at 65% of the window; {} messages now.",
        history.len()
    );
    Ok(())
}
