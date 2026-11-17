//! A host's own thresholds, against a scripted model: no network, no keys.
//! `cargo run -p haest --example threshold`; `cargo test` runs it too.
//!
//! A research agent whose host adds two thresholds beside haest's own:
//!
//! - at 60% of the model's window, the model is reminded to write down what
//!   it knows, once each time the context climbs past the mark;
//! - at the third model call of a turn, the human is warned (the model does
//!   not see it).
//!
//! The model looks things up three times, the context grows with each answer,
//! and each threshold fires once.

use std::sync::{Arc, Mutex};

use haest::{
    Agent, AgentDeps, AgentHooks, AgentOptions, BoxError, JsonMap, LoopEvent, ShutWorkspace, Tool,
    ToolContext, ToolError, ToolOutput, Tools, TurnCaps, Usage,
    core::{
        testing::{ScriptedProvider, text_answer, tool_answer},
        threshold::{Action, Mark, Meter, Reading, Rearm, Threshold, Thresholds},
    },
    serde_json::{Value, json},
};

/// The model's window, in tokens.
const WINDOW: u64 = 10_000;

/// At 60% of the window: write down what you know.
struct SaveNotes;

impl Threshold for SaveNotes {
    fn meter(&self) -> Meter {
        Meter::ContextTokens
    }
    fn mark(&self) -> Mark {
        Mark::share(0.6)
    }
    fn action(&self) -> Action {
        Action::Remind
    }
    fn rearm(&self) -> Rearm {
        // Again only after the context falls back under the mark (after a
        // compaction, say).
        Rearm::BelowMark
    }
    fn text(&self, r: &Reading) -> String {
        format!(
            "The context holds {} of {} tokens: write what you have found to NOTES.md before it is summarised.",
            r.value,
            r.cap.unwrap_or_default()
        )
    }
}

/// At the third model call of a turn: tell the human.
struct LongTurn;

impl Threshold for LongTurn {
    fn meter(&self) -> Meter {
        Meter::Steps
    }
    fn mark(&self) -> Mark {
        Mark::at(3.0)
    }
    fn action(&self) -> Action {
        Action::Warn
    }
    fn rearm(&self) -> Rearm {
        Rearm::NextTurn
    }
    fn text(&self, r: &Reading) -> String {
        format!("This turn has made {} model calls.", r.value)
    }
}

/// The host: what the human must hear goes to its own log.
#[derive(Default)]
struct Host {
    warnings: Mutex<Vec<String>>,
}

impl AgentHooks for Host {
    fn on_warning(&self, text: &str) {
        println!("(for the human) {text}");
        let mut warnings = self.warnings.lock().expect("the warnings' lock poisoned");
        warnings.push(text.to_string());
    }
}

/// Looks a topic up.
struct Lookup;

#[haest::async_trait]
impl Tool for Lookup {
    fn name(&self) -> &str {
        "Lookup"
    }
    fn description(&self) -> String {
        "Look a topic up.".into()
    }
    fn parameters(&self) -> Value {
        json!({"type": "object", "properties": {"topic": {"type": "string"}}})
    }
    fn kind(&self) -> &str {
        "research.lookup"
    }
    fn read_only(&self) -> bool {
        true
    }
    async fn execute(&self, input: JsonMap, _: &ToolContext) -> Result<ToolOutput, ToolError> {
        let topic = input.get("topic").and_then(Value::as_str).unwrap_or("?");
        Ok(ToolOutput::text(format!("Three pages on {topic}.")))
    }
}

/// One model answer: a lookup, from a request of `context` tokens.
fn lookup(id: &str, topic: &str, context: u64) -> haest::AssistantMessage {
    let mut msg = tool_answer(&[(id, "Lookup", json!({"topic": topic}))]);
    msg.usage = Usage {
        input: context,
        ..Usage::default()
    };
    msg
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), BoxError> {
    run().await
}

#[tokio::test]
async fn each_threshold_fires_once() -> Result<(), BoxError> {
    run().await
}

async fn run() -> Result<(), BoxError> {
    let provider = ScriptedProvider::new([
        lookup("a", "tides", 3_000),
        lookup("b", "currents", 6_500),
        lookup("c", "moons", 7_000),
        text_answer("Tides follow the moon; NOTES.md has the rest."),
    ]);
    let host = Arc::new(Host::default());
    // haest's own (the guard's nudges), then the host's.
    let thresholds = Thresholds::default().with(SaveNotes)?.with(LongTurn)?;
    let deps = AgentDeps::new(
        provider.resolver(),
        Tools::new().with(Lookup),
        "You research.",
        "/work",
        ShutWorkspace,
    )
    .with_hooks(host.clone())
    // The window is the cap a share of the context is taken of.
    .with_turn_caps(TurnCaps {
        context_window: WINDOW,
        ..TurnCaps::default()
    })
    .with_thresholds(thresholds);
    let mut agent = Agent::new(deps, AgentOptions::new("scripted:researcher"));

    let mut reminders = Vec::new();
    let mut sink = |ev: LoopEvent| match ev {
        LoopEvent::Guard { text } => {
            println!("(to the model) {text}");
            reminders.push(text);
        }
        LoopEvent::TextDelta { text } => println!("model: {text}"),
        _ => {}
    };
    agent.run("What moves the sea?", &mut sink).await;

    // Past 60% at 6,500 tokens, and not again at 7,000: it never fell under.
    assert_eq!(reminders.len(), 1);
    assert!(
        reminders[0].contains("6500 of 10000 tokens"),
        "{reminders:?}"
    );
    // The third call warned the human, once; the model never saw it.
    let warnings = host.warnings.lock().expect("the warnings' lock poisoned");
    assert_eq!(*warnings, ["This turn has made 3 model calls."]);
    let last = provider.requests().pop().ok_or("the model was called")?;
    assert!(!format!("{:?}", last.messages).contains("model calls"));
    Ok(())
}
