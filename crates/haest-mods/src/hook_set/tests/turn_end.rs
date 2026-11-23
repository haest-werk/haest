//! What `turn.end` hands a hook beyond the agent: the answer that ends the
//! turn, and whether a `turn.end` decision already kept this turn going,
//! the same for a hook inside a nested [`Mods`] built over its own
//! [`ModEnv`](crate::ModEnv).

use std::sync::Mutex;

use haest_core::{Agent, AssistantMessage};

use super::*;
use crate::{api::ModRef, decision::TurnEndDecision, payload::TurnEndPayload};

type Seen = Arc<Mutex<Vec<(String, bool)>>>;

/// The answer's text and `continued`, as a hook was handed them.
fn seen(p: &TurnEndPayload) -> (String, bool) {
    let answer: AssistantMessage =
        serde_json::from_value(p.answer.clone().expect("turn.end hands the answer")).unwrap();
    (answer.text(), p.continued)
}

/// A `turn.end` hook that writes down what it was handed and passes.
fn recorder(log: &Seen) -> Hooks {
    let log = log.clone();
    Hooks::new().on_turn_end(move |p: TurnEndPayload, _: CancellationToken| {
        log.lock().unwrap().push(seen(&p));
        async { Ok::<_, BoxError>(TurnEndDecision::default()) }
    })
}

/// A `turn.end` hook that keeps a turn going once, as a Claude Code stop
/// hook does with `stop_hook_active`: only when no decision did yet.
fn once_per_turn() -> Hooks {
    Hooks::new().on_turn_end(|p: TurnEndPayload, _: CancellationToken| async move {
        let decision = TurnEndDecision::default();
        Ok::<_, BoxError>(if p.continued {
            decision
        } else {
            decision.with_continue("check once more")
        })
    })
}

async fn two_turns(mods: &Mods) -> Agent {
    let p = ScriptedProvider::new(["first", "second", "third", "fourth"].map(text_answer));
    let mut agent = looped(&p, NoHooks, mods, Tools::new());
    for input in ["go", "again"] {
        let end = agent.run(input, &mut Vec::new()).await;
        assert_eq!(end.error, None);
    }
    assert_eq!(p.requests().len(), 4);
    agent
}

#[tokio::test]
async fn turn_end_hands_the_answer_and_whether_a_decision_already_kept_the_turn_going() {
    let log = Seen::default();
    let haest = env(TestUi::new(), WorkspaceMode::Sandbox);
    let mods = Mods::new(haest, [recorder(&log), once_per_turn()]);
    two_turns(&mods).await;
    // Each turn was kept going once; a new turn starts over.
    let expected = [
        ("first", false),
        ("second", true),
        ("third", false),
        ("fourth", true),
    ];
    assert_eq!(
        *log.lock().unwrap(),
        expected.map(|(t, c)| (t.to_string(), c))
    );
}

#[tokio::test]
async fn a_hook_in_a_nested_mods_on_its_own_env_sees_the_outer_decision() {
    let log = Seen::default();
    let inner = Mods::new(env(TestUi::new(), WorkspaceMode::Sandbox), [recorder(&log)]);
    let outer = env(TestUi::new(), WorkspaceMode::Sandbox);
    let mods = Mods::new(outer, [ModRef::new(inner), ModRef::new(once_per_turn())]);
    two_turns(&mods).await;
    let flags: Vec<bool> = log.lock().unwrap().iter().map(|(_, c)| *c).collect();
    assert_eq!(flags, [false, true, false, true]);
}
