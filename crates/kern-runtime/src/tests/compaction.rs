//! The host compacts before a model call that would overflow. What the
//! summary calls cost goes on the conversation's ledger once, whether the
//! summary came out or not.

use std::sync::{Arc, Mutex};

use crate::{
    Host,
    kern::{
        Agent, AgentDeps, AgentOptions, AssistantMessage, LoopEvent, Message, Provider,
        ResolvedModel, ShutWorkspace, StopReason, Tool, TurnEnd,
        ledger::{Ledger, LedgerCaps},
        testing::{ScriptedProvider, text_answer},
    },
    testkit::{host, settings},
};

/// A conversation on `host` whose last request filled the window, so the
/// next model call compacts first; its ledger has no cap.
fn full(host: &Arc<Host>, provider: Arc<dyn Provider>) -> (Agent, Arc<Mutex<Ledger>>) {
    let ledger = Arc::new(Mutex::new(Ledger::new(LedgerCaps::default(), host.clone())));
    let resolve = move |_: &str| -> Result<ResolvedModel, String> {
        Ok(ResolvedModel {
            provider: provider.clone(),
            model: "m".into(),
        })
    };
    let none: Vec<Arc<dyn Tool>> = Vec::new();
    let bare = AgentDeps::new(
        Arc::new(resolve),
        Arc::new(none),
        Arc::new(String::new()),
        "/w",
        Arc::new(ShutWorkspace),
    );
    let deps = AgentDeps {
        hooks: host.clone(),
        ledger: Some(ledger.clone()),
        ..bare
    };
    let mut agent = Agent::new(deps, AgentOptions::new("scripted:m"));
    let mut filled = text_answer("earlier answer");
    filled.usage.input = 1_000_000;
    agent.messages = vec![Message::user("earlier"), Message::Assistant(filled)];
    (agent, ledger)
}

async fn turn(agent: &mut Agent) -> TurnEnd {
    let mut events: Vec<LoopEvent> = Vec::new();
    agent.run("go", &mut events).await
}

/// A reply that is no summary (it says nothing), at a price.
fn priced_nothing(usd: f64) -> AssistantMessage {
    let mut m = AssistantMessage::start("scripted", "m");
    m.usage.cost_usd = Some(usd);
    m
}

#[tokio::test]
async fn a_failed_compaction_goes_on_the_conversations_bill_once() {
    let replies = ScriptedProvider::new([priced_nothing(0.25), priced_nothing(0.5)]);
    let (host, _) = host(settings());
    let (mut agent, ledger) = full(&host, replies.clone());
    let end = turn(&mut agent).await;
    assert_eq!(end.stop_reason, StopReason::Error, "{:?}", end.error);
    assert_eq!(replies.requests().len(), 2, "the warm and the cold summary");
    assert_eq!(ledger.lock().unwrap().usd, 0.75);
}
