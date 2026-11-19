//! Subagent budgets (`subagent_budgets`) on the hub: a budget only tightens
//! the caps, and its spend is held on the conversation's ledger while it
//! runs, so children running together cannot overshoot together. Off, nothing
//! of it shows. The time fit is the Agent tool's (`options.rs`).

use std::{sync::Arc, time::Duration};

use haest_core::caps::ConversationCap;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use super::{
    caps,
    lifecycle::{Lifecycle, Step},
    script::Script,
    support::{CALL_USD, Rig, call, now, say},
};
use crate::agents::{AgentError, AgentRef, MemberCaps, SpawnInput, SubagentBudget, SubagentCaps};

fn close(a: f64, b: f64) -> bool {
    (a - b).abs() < 1e-9
}

/// Budgets on: each subagent at most $0.40.
fn budgeted() -> SubagentCaps {
    SubagentCaps {
        usd: 0.4,
        max_concurrent: 8,
        budgets: true,
        ..caps()
    }
}

/// A subagent model that answers only once `gate` is cancelled.
fn gated(gate: &CancellationToken) -> Script {
    let gate = gate.clone();
    Box::new(move |_, _| {
        let gate = gate.clone();
        Box::pin(async move {
            gate.cancelled().await;
            say("done")
        })
    })
}

fn background(name: &str, budget: Option<Value>) -> Value {
    let mut v =
        json!({"description": name, "name": name, "prompt": "p", "run_in_background": true});
    if let Some(b) = budget {
        v["budget"] = b;
    }
    v
}

/// Every model call so far, main's and the subagents', at $0.01 each.
fn all_calls_usd(r: &Rig) -> f64 {
    let (main, sub) = *r.parent.provider.calls.lock().unwrap();
    (main + sub) as f64 * CALL_USD
}

fn cap_conversation(r: &Rig, usd: f64) {
    r.parent.ledger.set_cap(ConversationCap::Usd(usd));
}

fn starts(l: &Lifecycle) -> Vec<SpawnInput> {
    let steps = l.steps().into_iter();
    let starts = steps.filter_map(|s| match s {
        Step::Start { start, .. } => Some(start.input),
        Step::End { .. } => None,
    });
    starts.collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn children_started_together_hold_their_spend_together_and_cannot_overshoot_the_budget() {
    let release = CancellationToken::new();
    let mut r = Rig::new(
        now(|_, n| match n {
            0 => vec![
                call("a", "Agent", background("a", None)),
                call("b", "Agent", background("b", None)),
                call("c", "Agent", background("c", None)),
                call("d", "Agent", background("d", Some(json!({"usd": 0.1})))),
            ],
            _ => say("ok"),
        }),
        gated(&release),
        budgeted(),
    );
    cap_conversation(&r, 1.0);
    r.turn("go").await;
    let results = r.tool_results();
    let refused: Vec<bool> = results.iter().map(|(_, error)| *error).collect();
    // a and b hold $0.80 of what main's first call left ($0.99); c's $0.40 does
    // not fit the $0.19 left; d asks for less, and fits.
    assert_eq!(refused, [false, false, true, false]);
    let left = AgentError::OverBudget {
        asked: 0.4,
        available: 1.0 - CALL_USD - 0.8,
    };
    assert_eq!(results[2].0, left.to_string());
    assert_eq!(r.hub.running().len(), 3);
    {
        let l = &r.parent.ledger;
        assert!(close(l.held(), 0.9));
        assert!(l.usd() + l.held() <= 1.0, "held and spent fit the cap");
    }
    release.cancel();
    r.settle(|r| {
        r.hub.answers_due() == 0
            && r.texts().iter().filter(|t| t.contains("finished]")).count() == 3
    })
    .await;
    let l = &r.parent.ledger;
    assert_eq!(l.held(), 0.0, "every hold released");
    assert!(
        close(l.usd(), all_calls_usd(&r)),
        "only the real spend stays"
    );
}

/// The dollar amounts a refused budget shows, in order: what was asked (none
/// for a subagent with no spend cap), then what is left.
fn amounts(asked: f64, available: f64) -> Vec<String> {
    AgentError::OverBudget { asked, available }
        .to_string()
        .split_whitespace()
        .filter(|w| w.starts_with('$'))
        .map(|w| w.trim_end_matches([':', ';', ',']).to_string())
        .collect()
}

/// The budget a subagent asked for was printed as the float it was: one of
/// $0.10 + $0.20 read "a subagent's $0.30000000000000004 does not fit".
#[test]
fn a_refused_budget_says_both_amounts_in_cents() {
    assert_eq!(amounts(0.1 + 0.2, 0.2), ["$0.30", "$0.20"]);
}

/// What was left was rounded to the nearest cent, so it could round up to
/// the amount asked: "a subagent's $0.10 does not fit: $0.10 is left".
#[test]
fn a_refused_budget_says_what_is_left_rounded_down_and_a_tiny_ask_as_it_is() {
    assert_eq!(amounts(0.1, 0.0999), ["$0.10", "$0.09"]);
    assert_eq!(amounts(0.004, 0.003), ["$0.004", "$0.00"]);
    assert_eq!(amounts(0.0, 0.0999), ["$0.09"]);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_hold_lasts_while_its_child_runs_and_at_its_end_only_the_real_spend_stays() {
    let release = CancellationToken::new();
    let mut r = Rig::new(
        now(|_, n| match n {
            0 => vec![call(
                "a",
                "Agent",
                background("slow", Some(json!({"usd": 0.3}))),
            )],
            _ => say("ok"),
        }),
        gated(&release),
        budgeted(),
    );
    cap_conversation(&r, 1.0);
    r.turn("go").await;
    assert!(close(r.parent.ledger.held(), 0.3));
    release.cancel();
    r.settle(|r| r.hub.answers_due() == 0).await;
    let l = &r.parent.ledger;
    assert_eq!(l.held(), 0.0);
    assert!(close(l.usd(), all_calls_usd(&r)));
    assert!(close(l.available().unwrap(), 1.0 - all_calls_usd(&r)));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_budget_only_tightens_the_caps_and_an_absent_value_is_the_cap() {
    let l = Arc::new(Lifecycle::default());
    let mut r = Rig::with_lifecycle(
        now(|_, n| match n {
            0 => vec![call(
                "a",
                "Agent",
                json!({"description": "x", "prompt": "p", "budget": {"usd": 5, "minutes": 1}}),
            )],
            1 => vec![call(
                "b",
                "Agent",
                json!({"description": "y", "prompt": "p"}),
            )],
            2 => vec![call(
                "c",
                "Agent",
                json!({"description": "z", "prompt": "p", "budget": {"usd": 0}}),
            )],
            _ => say("ok"),
        }),
        now(|_, _| say("done")),
        budgeted(),
        l.clone(),
    );
    r.turn("go").await;
    let caps = budgeted();
    let budgets: Vec<_> = starts(&l).into_iter().map(|i| i.budget).collect();
    assert_eq!(
        budgets,
        [
            Some(SubagentBudget {
                usd: 0.4,
                time: Duration::from_secs(60)
            }),
            Some(SubagentBudget {
                usd: 0.4,
                time: caps.time
            }),
        ]
    );
    // The member runs under it: the caps a start sees, and its note.
    let Step::Start { start, .. } = &l.steps()[0] else {
        panic!()
    };
    let expected = MemberCaps::new(0.4, caps.steps, Duration::from_secs(60));
    assert_eq!(start.caps, expected);
    // A budget of nothing is the model's mistake, not "no cap".
    assert!(r.tool_results()[2].1);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_child_with_no_spend_cap_does_not_fit_a_capped_conversation_and_says_what_is_left() {
    let r = Rig::new(
        now(|_, _| say("x")),
        now(|_, _| say("done")),
        SubagentCaps {
            usd: 0.0,
            ..budgeted()
        },
    );
    cap_conversation(&r, 1.0);
    let ask = |usd: f64| SpawnInput {
        description: "d".into(),
        prompt: "p".into(),
        budget: Some(SubagentBudget {
            usd,
            time: Duration::from_secs(60),
        }),
        ..Default::default()
    };
    let main = AgentRef::main();
    let cancel = CancellationToken::new();
    let refused = r.hub.spawn(&main, ask(0.0), &cancel).await;
    assert_eq!(
        refused,
        Err(AgentError::OverBudget {
            asked: 0.0,
            available: 1.0
        })
    );
    let out = r.hub.spawn(&main, ask(0.2), &cancel).await;
    assert_eq!(out.unwrap().text, "done");
}

#[tokio::test(flavor = "multi_thread")]
async fn off_the_agent_tool_keeps_todays_schema_and_a_budget_changes_nothing() {
    let release = CancellationToken::new();
    let l = Arc::new(Lifecycle::default());
    let mut r = Rig::with_lifecycle(
        now(|_, n| match n {
            0 => vec![call(
                "a",
                "Agent",
                background("x", Some(json!({"usd": 0.01, "minutes": 1}))),
            )],
            _ => say("ok"),
        }),
        gated(&release),
        caps(),
        l.clone(),
    );
    cap_conversation(&r, 1.0);
    let agent = r.hub.tools_for(&AgentRef::main())[0].clone();
    let params = agent.parameters();
    let mut keys: Vec<&String> = params["properties"].as_object().unwrap().keys().collect();
    keys.sort();
    assert_eq!(
        keys,
        [
            "description",
            "model",
            "name",
            "prompt",
            "run_in_background"
        ]
    );
    r.turn("go").await;
    assert!(!r.tool_results()[0].1, "the call is not refused");
    let Step::Start { start, .. } = &l.steps()[0] else {
        panic!()
    };
    assert_eq!(start.input.budget, None);
    assert_eq!(start.caps, MemberCaps::from(caps()));
    assert_eq!(r.parent.ledger.held(), 0.0, "nothing held");
    release.cancel();
    r.settle(|r| r.hub.answers_due() == 0).await;
}
