//! The human's ALWAYS answers in a built conversation: every mod asks through
//! the runtime's gate, and the host can take an ALWAYS back.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use haest_core::fs::MemFs;
use haest_mods::{ApprovalAnswer, ApprovalRequest, Decision, Prompter};
use tokio_util::sync::CancellationToken;

use super::{hardcoded, openrouter_key};
use crate::{BuildOptions, RuntimeInputs, World, build, testkit::Recorded};

/// Answers in order, counting the prompts; out of answers = Deny.
#[derive(Clone, Default)]
struct Answers(Arc<Mutex<(Vec<ApprovalAnswer>, usize)>>);

#[async_trait]
impl Prompter for Answers {
    async fn prompt(&self, _: &ApprovalRequest) -> ApprovalAnswer {
        let mut state = self.0.lock().unwrap();
        state.1 += 1;
        if state.0.is_empty() {
            ApprovalAnswer::Deny
        } else {
            state.0.remove(0)
        }
    }
}

fn rm(command: &str) -> ApprovalRequest {
    ApprovalRequest::new("Bash", "haest wants to delete files")
        .with_input_preview(command)
        .with_key(command)
}

#[tokio::test]
async fn a_host_takes_an_always_back_in_a_built_conversation() {
    let answers = Answers::default();
    answers.0.lock().unwrap().0 = vec![ApprovalAnswer::AllowAlways, ApprovalAnswer::Deny];
    let inputs =
        RuntimeInputs::new(hardcoded(), World::files(MemFs::new())).with_keys(openrouter_key());
    let rt = build(
        inputs,
        Recorded::default(),
        answers.clone(),
        BuildOptions::default(),
    )
    .await
    .unwrap();
    let asks = rt.mods().env().ui().clone();
    let none = CancellationToken::new();

    assert_eq!(asks.ask(rm("rm -rf a"), &none).await, Decision::Allow);
    assert_eq!(asks.ask(rm("rm -rf a"), &none).await, Decision::Allow);
    rt.gate().revoke("Bash");
    assert_eq!(asks.ask(rm("rm -rf a"), &none).await, Decision::Deny);
    assert_eq!(
        answers.0.lock().unwrap().1,
        2,
        "asked again after the revoke"
    );
}
