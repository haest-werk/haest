//! The conversation's hooks around an answer are not run on a subagent's:
//! what they do (refuse an answer, keep a turn going) was meant for the
//! conversation's own agent.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use haest_core::{Agent, AgentHooks, AssistantMessage, BoxError, TurnOutcome};
use serde_json::json;
use tokio_util::sync::CancellationToken;

use super::{
    caps,
    support::{Rig, now, say, spawns},
};

/// The conversation's hooks: the answers they were asked about, and how
/// many turn ends.
#[derive(Default)]
struct Counting {
    answers: Mutex<Vec<String>>,
    turn_ends: Mutex<u32>,
}

#[async_trait]
impl AgentHooks for Counting {
    async fn after_model_call(
        &self,
        _: &mut Agent,
        answer: &AssistantMessage,
        _: &CancellationToken,
    ) -> Result<(), BoxError> {
        self.answers.lock().unwrap().push(answer.text());
        Ok(())
    }
    async fn before_turn_end(
        &self,
        _: &mut Agent,
        _: &TurnOutcome,
        _: &CancellationToken,
    ) -> Result<Option<String>, BoxError> {
        *self.turn_ends.lock().unwrap() += 1;
        Ok(None)
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn the_conversations_hooks_around_an_answer_are_not_run_on_a_subagents() {
    let mut r = Rig::new(
        spawns(json!({"description": "looker", "prompt": "p"}), "ok"),
        now(|_, _| say("found it")),
        caps(),
    );
    let host = Arc::new(Counting::default());
    let hooks = host.clone();
    r.adjust(move |d| d.with_hooks(hooks.clone()));
    r.turn("go").await;

    assert_eq!(r.ids("started").len(), 1);
    // Main's two answers (the spawn, then "ok"); not the subagent's.
    let answers = host.answers.lock().unwrap().clone();
    assert_eq!(answers.len(), 2, "{answers:?}");
    assert!(!answers.iter().any(|a| a == "found it"), "{answers:?}");
    assert_eq!(*host.turn_ends.lock().unwrap(), 1);
}
