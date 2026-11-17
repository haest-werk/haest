//! Wind-down in the loop: past the share, a tool that starts new work is
//! refused and the model hears it once; below it, or off, nothing changes.

use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use serde_json::json;

use super::*;
use crate::{
    agent::test_support::*,
    caps::{ConversationCaps, TurnCaps},
    events::LoopEvent,
    guard::TurnGuard,
    ledger::{Ledger, NoLedgerHooks},
    messages::{AssistantMessage, Message, Usage},
    remedy::{Limit, Remedies},
    testing::{ScriptedProvider, text_answer},
    threshold::Watch,
    tool::{ToolError, ToolOutput, ToolRef},
};

/// A conversation budget of `usd`.
fn ledger(usd: f64) -> Ledger {
    let caps = ConversationCaps {
        max_usd: usd,
        max_unpriced_tokens: 0,
    };
    Ledger::new(caps, NoLedgerHooks)
}

/// "Spawn": starts new work unless the turn is winding down, then refuses
/// with the fact. Every start is counted.
fn spawn_tool(started: Arc<Mutex<u32>>) -> ToolRef {
    FnTool::arc("Spawn", false, move |_, ctx| {
        let started = started.clone();
        async move {
            if let Some(w) = ctx.wind_down.check() {
                return Err(ToolError::Input(w.refusal("no new work")));
            }
            *started.lock().unwrap() += 1;
            Ok(ToolOutput::text("started"))
        }
    })
}

/// A call of `tool` that costs `usd`.
fn costs(id: &str, tool: &str, usd: f64) -> AssistantMessage {
    let msg = crate::testing::tool_answer(&[(id, tool, json!({}))]);
    let usage = Usage {
        cost_usd: Some(usd),
        ..Default::default()
    };
    with_usage(msg, &usage)
}

fn notices(events: &[LoopEvent]) -> usize {
    count(events, |e| matches!(e, LoopEvent::Guard { .. }))
}

fn refusals(events: &[LoopEvent]) -> usize {
    count(
        events,
        |e| matches!(e, LoopEvent::ToolResult { output, .. } if output.is_error),
    )
}

/// The agent the tests share: Spawn and Echo, `opts`, winding down at `at`.
fn agent(p: &ScriptedProvider, opts: Opts, at: f64) -> (crate::Agent, Arc<Mutex<u32>>) {
    let started = Arc::new(Mutex::new(0));
    let tools = vec![spawn_tool(started.clone()), echo_tool()];
    let opts = Opts {
        tools: Some(tools),
        ..opts
    };
    let (mut agent, _) = agent_with(p, opts);
    winding_down_at(&mut agent, at);
    (agent, started)
}

fn winding_down_at(agent: &mut crate::Agent, at: f64) {
    agent.map_deps(|d| {
        d.with_thresholds(Thresholds::default().wind_down(WindDown::at(at).unwrap()))
    });
}

/// What a tool asks of a turn that winds down at `at`, `elapsed` into it on
/// the host's clock, having spent `spent`: the guard's own watch.
fn crossed(
    at: WindDown,
    spent: &Usage,
    elapsed: Duration,
    caps: Option<&TurnCaps>,
    ledger: Option<&Ledger>,
    remedies: &Remedies,
) -> Option<WindingDown> {
    let now = Arc::new(AtomicU64::new(0));
    let clock = {
        let now = now.clone();
        move || now.load(Ordering::SeqCst)
    };
    let watch = Arc::new(Mutex::new(Watch::new(Thresholds::none().wind_down(at))));
    let guard = TurnGuard::new(Arc::new(clock), ledger.cloned(), remedies.clone())
        .with_thresholds(watch, 0);
    now.store(elapsed.as_millis() as u64, Ordering::SeqCst);
    guard.wind_down_watch(spent, caps.cloned()).check()
}

/// Spend crosses 80% of the conversation budget on the first call; three
/// steps follow, with a spawn on each side of the crossing.
fn crossing_script() -> Vec<AssistantMessage> {
    vec![
        costs("a", "Spawn", 0.5),
        costs("b", "Spawn", 0.4),
        costs("c", "Echo", 0.0),
        costs("d", "Spawn", 0.0),
        text_answer("done"),
    ]
}

#[tokio::test]
async fn past_the_share_of_the_conversation_budget_spawns_are_refused_and_the_model_hears_it_once()
{
    let p = ScriptedProvider::new(crossing_script());
    let opts = Opts {
        ledger: Some(ledger(1.0)),
        ..Default::default()
    };
    let (mut agent, started) = agent(&p, opts, 0.8);
    let events = collect(&mut agent, "go").await;
    // $0.50 is below $0.80: the first spawn starts; from $0.90 on, none does.
    assert_eq!(*started.lock().unwrap(), 1);
    assert_eq!(refusals(&events), 2);
    assert_eq!(notices(&events), 1);
    // The request after the crossing carried the notice to the model.
    let sent = &p.requests()[2].messages;
    assert!(matches!(sent.last(), Some(Message::User(_))));
}

#[tokio::test]
async fn past_the_share_of_a_turn_cap_spawns_are_refused_and_the_model_hears_it_once() {
    // Below the guard's own nudge (half the cap), so the only notice is wind-down's.
    let p = ScriptedProvider::new([
        costs("a", "Spawn", 0.45),
        costs("b", "Spawn", 0.0),
        costs("c", "Echo", 0.0),
        text_answer("done"),
    ]);
    let (mut agent, started) = agent(&p, capped(|l| l.max_turn_usd = 1.0), 0.4);
    let events = collect(&mut agent, "go").await;
    assert_eq!(*started.lock().unwrap(), 0);
    assert_eq!(refusals(&events), 2);
    assert_eq!(notices(&events), 1);
}

#[tokio::test]
async fn a_spawn_after_one_in_the_same_answer_paid_past_the_share_is_refused() {
    // The first call's own spend crosses the turn's share mid-answer.
    let paid = FnTool::arc("Paid", false, |_, _| async {
        Ok(ToolOutput {
            content: "done".into(),
            cost_usd: Some(0.9),
            ..Default::default()
        })
    });
    let started = Arc::new(Mutex::new(0));
    let both = crate::testing::tool_answer(&[("a", "Paid", json!({})), ("b", "Spawn", json!({}))]);
    let p = ScriptedProvider::new([both, text_answer("done")]);
    let opts = Opts {
        tools: Some(vec![paid, spawn_tool(started.clone())]),
        ..capped(|l| l.max_turn_usd = 2.0)
    };
    let (mut agent, _) = agent_with(&p, opts);
    winding_down_at(&mut agent, 0.4);
    let events = collect(&mut agent, "go").await;
    assert_eq!(*started.lock().unwrap(), 0);
    assert_eq!(refusals(&events), 1);
}

#[tokio::test]
async fn below_the_share_nothing_changes() {
    let p = ScriptedProvider::new(crossing_script());
    let opts = Opts {
        ledger: Some(ledger(10.0)),
        ..Default::default()
    };
    let (mut agent, started) = agent(&p, opts, 0.8);
    let events = collect(&mut agent, "go").await;
    assert_eq!(*started.lock().unwrap(), 3);
    assert_eq!(refusals(&events), 0);
    assert_eq!(notices(&events), 0);
}

#[tokio::test]
async fn off_behaves_as_without_wind_down_even_at_the_edge_of_the_budget() {
    let kinds = |at: Option<f64>| async move {
        let p = ScriptedProvider::new(crossing_script());
        let opts = Opts {
            ledger: Some(ledger(1.0)),
            ..Default::default()
        };
        let (mut agent, started) = match at {
            Some(at) => agent(&p, opts, at),
            // Never set: AgentDeps::new's default.
            None => {
                let started = Arc::new(Mutex::new(0));
                let tools = vec![spawn_tool(started.clone()), echo_tool()];
                let opts = Opts {
                    tools: Some(tools),
                    ..opts
                };
                (agent_with(&p, opts).0, started)
            }
        };
        let events = collect(&mut agent, "go").await;
        let kinds: Vec<String> = events
            .iter()
            .map(|e| serde_json::to_value(e).unwrap()["type"].to_string())
            .collect();
        (kinds, *started.lock().unwrap(), p.requests().len())
    };
    let (default_kinds, default_started, default_calls) = kinds(None).await;
    let (off_kinds, off_started, off_calls) = kinds(Some(0.0)).await;
    assert_eq!(off_kinds, default_kinds);
    assert_eq!((off_started, off_calls), (default_started, default_calls));
    // Every spawn started and nothing was said: as before wind-down existed.
    assert_eq!(off_started, 3);
    assert!(!off_kinds.iter().any(|k| k.contains("guard")));
}

#[tokio::test]
async fn a_turn_that_starts_past_the_conversations_share_is_told_once_and_spawns_stay_refused() {
    let l = ledger(1.0);
    let p = ScriptedProvider::new([
        costs("a", "Echo", 0.9),
        text_answer("first"),
        costs("b", "Spawn", 0.0),
        costs("c", "Spawn", 0.0),
        text_answer("second"),
    ]);
    let opts = Opts {
        ledger: Some(l),
        ..Default::default()
    };
    let (mut agent, started) = agent(&p, opts, 0.8);
    let first = collect(&mut agent, "one").await;
    assert_eq!(notices(&first), 1);
    let second = collect(&mut agent, "two").await;
    assert_eq!(notices(&second), 1);
    assert_eq!(refusals(&second), 2);
    assert_eq!(*started.lock().unwrap(), 0);
}

#[test]
fn wall_time_past_the_share_on_the_hosts_clock_winds_down() {
    let caps = turn_caps(|l| l.max_turn_time = Duration::from_secs(100));
    let at = WindDown::at(0.5).unwrap();
    let none = Remedies::default();
    let past = |secs| {
        crossed(
            at,
            &Usage::default(),
            Duration::from_secs(secs),
            Some(&caps),
            None,
            &none,
        )
    };
    assert_eq!(past(49), None);
    let w = past(50).unwrap();
    assert_eq!(w.limit, Some(Limit::TurnTime(Duration::from_secs(100))));
}

#[test]
fn a_subagents_caps_name_its_own_settings_and_get_the_hosts_remedy_for_them() {
    let caps = TurnCaps {
        subagent: true,
        ..turn_caps(|l| l.max_turn_usd = 1.0)
    };
    let remedies = Remedies::new(|limit| match limit {
        Limit::SubagentUsd(_) => Some("REMEDY".into()),
        _ => None,
    });
    let spent = Usage {
        cost_usd: Some(0.9),
        ..Default::default()
    };
    let at = WindDown::at(0.8).unwrap();
    let w = crossed(at, &spent, Duration::ZERO, Some(&caps), None, &remedies).unwrap();
    assert_eq!(w.limit, Some(Limit::SubagentUsd(1.0)));
    let refusal = w.refusal("no new subagents");
    assert!(refusal.contains("subagent_usd"), "{refusal}");
    assert!(refusal.ends_with(" REMEDY"), "{refusal}");
}

#[test]
fn a_cap_that_is_off_is_never_watched() {
    let at = WindDown::at(0.1).unwrap();
    let spent = Usage {
        cost_usd: Some(1e9),
        output: 1_000_000_000,
        ..Default::default()
    };
    let none = Remedies::default();
    let w = crossed(
        at,
        &spent,
        Duration::from_secs(1e6 as u64),
        Some(&TurnCaps::default()),
        Some(&ledger(0.0)),
        &none,
    );
    assert_eq!(w, None);
}

#[test]
fn a_share_outside_zero_to_one_is_refused() {
    for bad in [-0.1, 1.5, f64::NAN, f64::INFINITY] {
        assert!(WindDown::at(bad).is_err(), "{bad}");
    }
    assert!(WindDown::at(1.0).unwrap().is_on());
    assert!(!WindDown::default().is_on());
}
