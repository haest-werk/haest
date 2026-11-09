use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
    time::Duration,
};

use async_trait::async_trait;
use tokio_util::sync::CancellationToken;

use super::*;

/// Answers prompts in order; out of answers = never answers.
#[derive(Default)]
struct Human {
    answers: Mutex<VecDeque<Answer>>,
    prompts: Mutex<Vec<ApprovalRequest>>,
    receipts: Mutex<Vec<ApprovalRequest>>,
}

impl Human {
    fn answering(answers: impl IntoIterator<Item = Answer>) -> Arc<Self> {
        Arc::new(Self {
            answers: Mutex::new(answers.into_iter().collect()),
            ..Default::default()
        })
    }
    fn prompts(&self) -> usize {
        self.prompts.lock().unwrap().len()
    }
}

#[async_trait]
impl Prompter for Arc<Human> {
    async fn prompt(&self, req: &ApprovalRequest) -> Answer {
        self.prompts.lock().unwrap().push(req.clone());
        let next = self.answers.lock().unwrap().pop_front();
        match next {
            Some(a) => a,
            None => std::future::pending().await,
        }
    }
    fn auto_approved(&self, req: &ApprovalRequest) {
        self.receipts.lock().unwrap().push(req.clone());
    }
}

fn bash(command: &str) -> ApprovalRequest {
    ApprovalRequest {
        tool_name: "Bash".into(),
        description: "kern wants to run a command that deletes files recursively or by force"
            .into(),
        input_preview: command.into(),
        tool_use_id: None,
        key: Some(command.into()),
    }
}

#[tokio::test]
async fn always_covers_that_exact_command_not_every_later_irreversible_one() {
    let human = Human::answering([Answer::AllowAlways]);
    let gate = ApprovalGate::new(human.clone());
    let none = CancellationToken::new();
    assert_eq!(
        gate.ask(bash("rm -rf node_modules"), &none).await,
        Decision::Allow
    );
    assert_eq!(
        gate.ask(bash("rm -rf node_modules"), &none).await,
        Decision::Allow
    );
    assert_eq!(human.prompts(), 1, "the second ask answered itself");
    assert_eq!(human.receipts.lock().unwrap().len(), 1);

    let leave = CancellationToken::new();
    let other = {
        let leave = leave.clone();
        let gate = &gate;
        async move { gate.ask(bash("rm -rf ~"), &leave).await }
    };
    let (decision, ()) = tokio::join!(other, async {
        tokio::task::yield_now().await;
        leave.cancel();
    });
    assert_eq!(human.prompts(), 2, "a different command prompts again");
    assert_eq!(decision, Decision::Deny);
}

#[tokio::test]
async fn an_ask_under_an_already_interrupted_turn_is_a_deny_at_once_never_a_prompt() {
    let human = Human::answering([Answer::Allow]);
    let gate = ApprovalGate::new(human.clone());
    let interrupted = CancellationToken::new();
    interrupted.cancel();
    assert_eq!(
        gate.ask(bash("rm -rf x"), &interrupted).await,
        Decision::Deny
    );
    assert_eq!(human.prompts(), 0);
}

#[tokio::test]
async fn an_ask_nobody_answers_is_a_deny_after_the_timeout() {
    let human = Human::answering([]);
    let gate = ApprovalGate::new(human.clone()).with_timeout(Duration::from_millis(20));
    let decision = gate.ask(bash("rm -rf x"), &CancellationToken::new()).await;
    assert_eq!(decision, Decision::Deny);
    assert_eq!(human.prompts(), 1);
}

#[tokio::test]
async fn revoking_always_for_a_tool_makes_its_asks_prompt_again() {
    let human = Human::answering([Answer::AllowAlways, Answer::Deny]);
    let gate = ApprovalGate::new(human.clone());
    let none = CancellationToken::new();
    assert_eq!(gate.ask(bash("rm -rf a"), &none).await, Decision::Allow);
    gate.revoke("Bash");
    assert_eq!(gate.ask(bash("rm -rf a"), &none).await, Decision::Deny);
    assert_eq!(human.prompts(), 2);
}
