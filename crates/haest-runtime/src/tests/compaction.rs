//! The host compacts before a model call that would overflow. What the
//! summary calls cost goes on the conversation's ledger once, whether the
//! summary came out or not; an Esc while it is written ends the turn as
//! cancelled, with no failure shown.

use std::sync::{Arc, OnceLock};

use crate::{
    RuntimeHooks,
    haest::{
        Agent, AgentControl, AgentDeps, AgentOptions, AssistantMessage, LoopEvent, Message,
        ModelError, Provider, ProviderRef, ProviderRequest, ResolvedModel, ShutWorkspace,
        StopReason, TurnOutcome,
        core::caps::ConversationCaps,
        core::ledger::Ledger,
        core::provider::ProviderStream,
        core::testing::{ScriptedProvider, text_answer},
    },
    testkit::{hooks, settings},
};
use haest_core::Tools;

/// A conversation on `host` whose last request filled the window, so the
/// next model call compacts first; its ledger has no cap.
fn full(hooks: &Arc<RuntimeHooks>, provider: impl Into<ProviderRef>) -> (Agent, Ledger) {
    let provider = provider.into();
    let ledger = Ledger::new(ConversationCaps::default(), hooks.clone());
    let resolve = move |_: &str| -> Result<ResolvedModel, ModelError> {
        Ok(ResolvedModel::new(provider.clone(), "m"))
    };
    let deps = AgentDeps::new(resolve, Tools::new(), "", "/w", ShutWorkspace)
        .with_hooks(hooks.clone())
        .with_services(hooks.clone())
        .with_facts(hooks.clone())
        .with_turn_caps(hooks.clone())
        .with_ledger(ledger.clone());
    let mut agent = Agent::new(deps, AgentOptions::new("scripted:m"));
    let mut filled = text_answer("earlier answer");
    filled.usage.input = 1_000_000;
    agent.replace_history(vec![Message::user("earlier"), Message::Assistant(filled)]);
    (agent, ledger)
}

async fn turn(agent: &mut Agent) -> TurnOutcome {
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
    let (hooks, _) = hooks(settings());
    let (mut agent, ledger) = full(&hooks, replies.clone());
    let end = turn(&mut agent).await;
    assert_eq!(end.stop_reason, StopReason::Error, "{:?}", end.error);
    assert_eq!(replies.requests().len(), 2, "the warm and the cold summary");
    assert_eq!(ledger.usd(), 0.75);
}

/// The human presses Esc as each request goes out, once `control` is set.
struct EscOnEveryCall {
    replies: ScriptedProvider,
    control: OnceLock<AgentControl>,
}

impl Provider for EscOnEveryCall {
    fn id(&self) -> &str {
        "scripted"
    }

    fn stream(&self, req: ProviderRequest) -> ProviderStream {
        if let Some(control) = self.control.get() {
            control.abort();
        }
        self.replies.stream(req)
    }
}

#[tokio::test]
async fn an_esc_while_the_summary_is_written_ends_the_turn_cancelled_with_no_failure_shown() {
    let replies = ScriptedProvider::new([text_answer("SUMMARY"), text_answer("never")]);
    let esc = Arc::new(EscOnEveryCall {
        replies: replies.clone(),
        control: OnceLock::new(),
    });
    let (hooks, ui) = hooks(settings());
    let (mut agent, _) = full(&hooks, esc.clone());
    let _ = esc.control.set(agent.control());
    let end = turn(&mut agent).await;
    assert_eq!(end, TurnOutcome::new(StopReason::Aborted));
    assert_eq!(ui.warnings(), Vec::<String>::new(), "no failure shown");
    assert_eq!(replies.requests().len(), 1, "the summary call only");
}
