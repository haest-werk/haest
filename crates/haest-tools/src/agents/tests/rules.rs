//! What the Rust hub defends beyond the ported tests: the spend rule (the
//! child's cost reaches the parent's turn, the ledger is charged once), and
//! StopAgent plus the post office's refusals.

use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use haest_core::LoopEvent;
use serde_json::json;
use tokio::sync::Notify;

use super::{
    caps,
    support::{CALL_USD, Rig, agent, now, say, spawns},
};
use crate::agents::{AgentDirectory, AgentError, AgentRef};

#[tokio::test(flavor = "multi_thread")]
async fn a_childs_spend_counts_in_the_parents_turn_and_is_charged_to_the_ledger_once() {
    let mut r = Rig::new(
        spawns(json!({"description": "x", "prompt": "p"}), "ok"),
        now(|_, _| say("done")),
        caps(),
    );
    r.turn("go").await;
    let child = r.events.iter().find_map(|e| match e {
        LoopEvent::ToolResult { output, .. } => output.child_cost_usd,
        _ => None,
    });
    assert_eq!(child, Some(CALL_USD));
    let turn = r.events.iter().find_map(|e| match e {
        LoopEvent::TurnEnd { usage, .. } => usage.cost_usd,
        _ => None,
    });
    let close = |a: f64, b: f64| (a - b).abs() < 1e-9;
    // Main's two calls and the child's one, in the turn and in the ledger alike.
    assert!(close(turn.unwrap(), 3.0 * CALL_USD));
    assert!(close(r.parent.ledger.usd(), 3.0 * CALL_USD));
}

#[tokio::test(flavor = "multi_thread")]
async fn stop_ends_a_running_agent_and_the_post_office_refuses_who_is_not_running() {
    let mut r = Rig::new(
        spawns(
            json!({"description": "slow", "name": "slow", "prompt": "p", "run_in_background": true}),
            "ok",
        ),
        Box::new(|_, _| Box::pin(std::future::pending())),
        caps(),
    );
    r.turn("go").await;
    let main = AgentRef::main();
    assert!(r.deliver(agent(&main.name), "slow", "hurry").is_ok());
    assert!(r.hub.stop("slow"));
    assert!(!r.hub.stop("slow"), "stopped once");
    let finished = |t: &String| t.starts_with("[Agent \"slow\" finished]");
    r.settle(|r| r.texts().iter().any(finished)).await;
    let answer = r.texts().into_iter().find(finished).unwrap();
    assert!(answer.contains("stopped early"));
    assert!(
        answer.contains("hurry"),
        "a message it never read comes back with its answer"
    );
    let refused = |to: &str| r.deliver(agent(&main.name), to, "x").unwrap_err();
    assert!(matches!(refused("slow"), AgentError::Finished { running, .. } if running == ["main"]));
    assert!(matches!(refused("ghost"), AgentError::UnknownAgent { name, .. } if name == "ghost"));
}

// A one-shot host waits for background work before it exits. `running()` is
// the wrong thing to wait on: the agent leaves it BEFORE its answer is handed
// back, so a host that sees it empty can exit with the answer still in flight.
#[tokio::test(flavor = "multi_thread")]
async fn a_background_answer_stays_due_until_it_has_reached_main() {
    let release = Arc::new(Notify::new());
    let wait = release.clone();
    let mut r = Rig::new(
        spawns(
            json!({"description": "slow", "name": "slow", "prompt": "p", "run_in_background": true}),
            "ok",
        ),
        Box::new(move |_, _| {
            let wait = wait.clone();
            Box::pin(async move {
                wait.notified().await;
                say("found it")
            })
        }),
        caps(),
    );
    assert_eq!(r.hub.answers_due(), 0);
    r.turn("go").await;
    assert_eq!(r.hub.answers_due(), 1);
    release.notify_one();
    let end = Instant::now() + Duration::from_secs(5);
    while r.hub.answers_due() > 0 && Instant::now() < end {
        tokio::task::yield_now().await;
    }
    assert_eq!(r.hub.answers_due(), 0);
    let answer = (r.inbox.try_recv()).expect("in main's inbox by the time it is no longer due");
    assert!(answer.contains("found it"));
}
