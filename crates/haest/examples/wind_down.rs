//! Wind-down, against a scripted model: no network, no keys.
//! `cargo run -p haest --example wind_down`; `cargo test` runs it too.
//!
//! A conversation with a $1 budget that winds down at 80% (`wind_down_at =
//! 0.8`). The model starts a subagent at $0.50; at $0.90 its next Agent call
//! is refused, saying what was spent of which cap and the host's remedy, and
//! the model is told once to conclude. It answers with what it has.

use std::sync::{Arc, Mutex};

use haest::{
    AgentDeps, AgentOptions, LoopEvent, ShutWorkspace, Usage,
    core::{
        Agent,
        caps::{ConversationCap, ConversationCaps},
        ledger::{Ledger, LedgerHooks, LedgerSnapshot},
        remedy::{Limit, Remedies},
        testing::{ScriptedProvider, text_answer, tool_answer},
        threshold::Thresholds,
        wind_down::WindDown,
    },
    settings::Settings,
    tools::{AgentDirectory, AgentError, AgentRef, SpawnInput, SpawnOutcome, agent_tools},
};
use serde_json::json;
use tokio_util::sync::CancellationToken;

/// The host's subagents: here each one answers at once, and is counted.
#[derive(Default)]
struct Helpers(Mutex<Vec<String>>);

#[haest::async_trait]
impl AgentDirectory for Helpers {
    async fn spawn(
        &self,
        _from: &AgentRef,
        input: SpawnInput,
        _cancel: CancellationToken,
    ) -> Result<SpawnOutcome, AgentError> {
        self.0.lock().unwrap().push(input.description.clone());
        let text = format!("{}: nothing unusual", input.description);
        Ok(SpawnOutcome {
            text,
            cost_usd: 0.0,
        })
    }
    fn deliver(&self, _from: &str, to: &str, _text: &str) -> Result<String, AgentError> {
        Err(AgentError::Refused(format!("\"{to}\" is not running")))
    }
    fn stop(&self, _name: &str) -> bool {
        false
    }
}

struct Quiet;
impl LedgerHooks for Quiet {
    fn persist(&self, _: &LedgerSnapshot) {}
    fn warn(&self, text: &str) {
        println!("(for the human) {text}");
    }
}

/// One model answer: an Agent call that costs `usd`.
fn spawn_at(id: &str, description: &str, usd: f64) -> haest::AssistantMessage {
    let brief = json!({"description": description, "prompt": "Look around."});
    let mut msg = tool_answer(&[(id, "Agent", brief)]);
    msg.usage = Usage {
        cost_usd: Some(usd),
        ..Usage::default()
    };
    msg
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    run().await;
}

#[tokio::test]
async fn the_spawn_past_the_share_is_refused_and_the_model_told_once() {
    run().await;
}

async fn run() {
    // The setting as a host writes it out (haest-config reads `wind_down_at`
    // into the same field).
    let settings = Settings {
        wind_down_at: WindDown::at(0.8).expect("a share from 0 to 1"),
        ..Settings::bare("/work", "/home/me")
    };

    let provider = ScriptedProvider::new([
        spawn_at("a", "scout the logs", 0.50),
        spawn_at("b", "scout the metrics", 0.40),
        text_answer("From the logs: nothing unusual. I stopped there: the budget is nearly spent."),
    ]);
    let helpers = Arc::new(Helpers::default());
    let tools = agent_tools(helpers.clone(), AgentRef::main(), true);
    // The host's way past the budget, added to haest's refusal.
    let remedies = Remedies::new(|limit| match limit {
        Limit::Conversation(ConversationCap::Usd(cap)) => {
            Some(format!("Ask the human to raise the budget past ${cap}."))
        }
        _ => None,
    });
    let budget = ConversationCaps {
        max_usd: 1.0,
        max_unpriced_tokens: 0,
    };
    let ledger = Ledger::new(budget, Arc::new(Quiet)).with_remedies(remedies.clone());
    let deps = AgentDeps::new(
        provider.resolver(),
        tools,
        "You lead a small team.",
        "/work",
        ShutWorkspace,
    )
    .with_ledger(Arc::new(Mutex::new(ledger)))
    .with_remedies(remedies)
    // Wind-down is a set of thresholds, beside the guard's nudges.
    .with_thresholds(Thresholds::default().wind_down(settings.wind_down_at));
    let mut agent = Agent::new(deps, AgentOptions::new("scripted:lead"));

    let (mut refused, mut notices) = (0, 0);
    let mut sink = |ev: LoopEvent| match ev {
        LoopEvent::ToolResult { output, .. } if output.is_error => {
            refused += 1;
            println!("refused: {}", output.content);
        }
        LoopEvent::ToolResult { output, .. } => println!("subagent: {}", output.content),
        LoopEvent::Guard { text } => {
            notices += 1;
            println!("notice: {text}");
        }
        LoopEvent::TextDelta { text } => println!("model: {text}"),
        _ => {}
    };
    agent
        .run("Find out what broke last night.", &mut sink)
        .await;

    assert_eq!(*helpers.0.lock().unwrap(), ["scout the logs"]);
    assert_eq!(refused, 1, "the spawn past 80% was refused");
    assert_eq!(notices, 1, "the model was told once");
}
