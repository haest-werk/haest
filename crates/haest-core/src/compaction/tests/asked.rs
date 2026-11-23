//! What a caller adds to a compaction: its own instructions reach the
//! summary request, and the agent's hooks hear the compaction it made.

use std::sync::Mutex;

use super::*;
use crate::{deps::AgentHooks, messages::UserMessage, provider::ProviderRequest};

/// Records every compaction it hears of.
#[derive(Default)]
struct Saves(Mutex<Vec<(String, usize)>>);

impl AgentHooks for Saves {
    fn on_compaction(&self, result: &CompactionResult) {
        let heard = (result.summary.clone(), result.kept);
        self.0.lock().unwrap().push(heard);
    }
}

/// The text of the last message of a request: the summary instruction.
fn asked(req: &ProviderRequest) -> String {
    match req.messages.last() {
        Some(Message::User(UserMessage { content, .. })) => content.clone(),
        other => panic!("no instruction last: {other:?}"),
    }
}

#[tokio::test]
async fn the_callers_instructions_reach_the_warm_and_the_cold_summary_request() {
    let focus = "Keep every file path; drop the jokes.";
    let p = ScriptedProvider::new([empty_answer(), text_answer("COLD")]);
    let mut agent = agent_with(&p);
    let cfg = CompactionOptions {
        instructions: Some(focus.into()),
        ..CFG
    };
    let res = compact(&mut agent, &CancellationToken::new(), &cfg).await;
    assert_eq!(res.unwrap().unwrap().path, SummaryPath::Cold);
    let requests = p.requests();
    assert_eq!(requests.len(), 2);
    assert!(requests.iter().all(|r| asked(r).contains(focus)));

    // Without instructions, neither request carries them.
    let p = ScriptedProvider::new([text_answer("WARM")]);
    let mut agent = agent_with(&p);
    compact(&mut agent, &CancellationToken::new(), &CFG)
        .await
        .unwrap();
    assert!(!asked(&p.requests()[0]).contains(focus));
}

#[tokio::test]
async fn the_hooks_hear_the_compaction_that_was_made_and_none_that_failed() {
    let saves = Arc::new(Saves::default());
    let p = ScriptedProvider::new([text_answer("SUMMARY")]);
    let mut agent = agent_with(&p);
    agent.map_deps(|d| d.with_hooks(saves.clone()));
    let res = compact(&mut agent, &CancellationToken::new(), &CFG)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        *saves.0.lock().unwrap(),
        [("SUMMARY".to_string(), res.kept)]
    );
    assert_eq!(agent.history().len(), res.kept + 1);

    let failing = ScriptedProvider::new([empty_answer(), empty_answer()]);
    let mut agent = agent_with(&failing);
    let quiet = Arc::new(Saves::default());
    agent.map_deps(|d| d.with_hooks(quiet.clone()));
    assert!(
        compact(&mut agent, &CancellationToken::new(), &CFG)
            .await
            .is_err()
    );
    assert!(quiet.0.lock().unwrap().is_empty());
}
