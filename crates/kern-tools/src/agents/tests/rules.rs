//! What the Rust hub defends beyond the ported tests: the spend rule (the
//! child's cost reaches the parent's turn, the ledger is charged once), and
//! StopAgent plus the post office's refusals.

use kern_core::LoopEvent;
use serde_json::json;

use super::{
    caps,
    support::{CALL_USD, Rig, call, now, say},
};
use crate::agents::{AgentDirectory, AgentRef};

#[tokio::test(flavor = "multi_thread")]
async fn a_childs_spend_counts_in_the_parents_turn_and_is_charged_to_the_ledger_once() {
    let mut r = Rig::new(
        now(|_, n| match n {
            0 => vec![call(
                "a",
                "Agent",
                json!({"description": "x", "prompt": "p"}),
            )],
            _ => say("ok"),
        }),
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
    assert!(close(r.host.ledger.lock().unwrap().usd, 3.0 * CALL_USD));
}

#[tokio::test(flavor = "multi_thread")]
async fn stop_ends_a_running_agent_and_the_post_office_refuses_who_is_not_running() {
    let mut r = Rig::new(
        now(|_, n| match n {
            0 => vec![call(
                "a",
                "Agent",
                json!({"description": "slow", "name": "slow", "prompt": "p", "run_in_background": true}),
            )],
            _ => say("ok"),
        }),
        Box::new(|_, _| Box::pin(std::future::pending())),
        caps(),
    );
    r.turn("go").await;
    let main = AgentRef::main();
    assert!(r.hub.deliver(&main.name, "slow", "hurry").is_ok());
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
    let refused = |to: &str| r.hub.deliver(&main.name, to, "x").unwrap_err();
    assert!(refused("slow").contains("finished"));
    assert!(refused("ghost").contains("no agent"));
}
