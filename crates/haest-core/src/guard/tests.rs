//! A haest that loops must cost a known amount: nudged at half a cap, stopped at it.

use std::{
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use serde_json::{Value, json};

use super::*;
use crate::{
    agent::test_support::*,
    events::LoopEvent,
    messages::{Message, StopReason},
    testing::{ScriptedProvider, text_answer},
    tool::ToolOutput,
};

fn same(n: usize) -> Vec<crate::messages::AssistantMessage> {
    (0..n)
        .map(|i| echo(&format!("t{i}"), json!({"v": "x"})))
        .collect()
}

fn guards(events: &[LoopEvent]) -> usize {
    count(events, |e| matches!(e, LoopEvent::Guard { .. }))
}

#[tokio::test]
async fn the_same_call_with_the_same_result_is_nudged_at_half_the_cap_and_stopped_at_the_cap() {
    let p = ScriptedProvider::new(same(20));
    let (mut agent, _) = agent_with(&p, capped(|l| l.max_repeats = 4));
    let events = collect(&mut agent, "go").await;
    assert_eq!(p.requests().len(), 4);
    assert_eq!(end(&events).0, StopReason::MaxTokens);
    assert_eq!(guards(&events), 1);
}

#[tokio::test]
async fn the_nudge_reaches_the_model_as_input_and_the_human_as_a_notice_never_as_their_own_message()
{
    let mut script = same(3);
    script.push(text_answer("done"));
    let p = ScriptedProvider::new(script);
    let (mut agent, _) = agent_with(&p, capped(|l| l.max_repeats = 4));
    let events = collect(&mut agent, "go").await;
    assert_eq!(count(&events, |e| matches!(e, LoopEvent::User { .. })), 1);
    let users = agent
        .history()
        .iter()
        .filter(|m| matches!(m, Message::User(_)))
        .count();
    assert_eq!(users, 2);
    // The request after the nudge carried it.
    let sent_after = &p.requests()[2].messages;
    assert!(matches!(sent_after.last(), Some(Message::User(_))));
}

#[tokio::test]
async fn a_call_whose_result_changes_is_progress_not_a_loop() {
    let n = Arc::new(AtomicU64::new(0));
    let counter = FnTool::arc("Echo", false, move |_, _| {
        let n = n.fetch_add(1, Ordering::SeqCst);
        async move { Ok(ToolOutput::text(n.to_string())) }
    });
    let mut script = same(10);
    script.push(text_answer("done"));
    let p = ScriptedProvider::new(script);
    let (mut agent, _) = agent_with(
        &p,
        Opts {
            tools: Some(vec![counter]),
            ..capped(|l| l.max_repeats = 2)
        },
    );
    let events = collect(&mut agent, "go").await;
    assert_eq!(p.requests().len(), 11);
    assert_eq!(end(&events).0, StopReason::EndTurn);
}

#[tokio::test]
async fn key_order_does_not_hide_a_repeat() {
    let p = ScriptedProvider::new([
        echo("a", json!({"x": 1, "y": 2})),
        echo("b", json!({"y": 2, "x": 1})),
        echo("c", json!({"x": 1, "y": 2})),
    ]);
    let (mut agent, _) = agent_with(&p, capped(|l| l.max_repeats = 2));
    collect(&mut agent, "go").await;
    assert_eq!(p.requests().len(), 2);
}

#[tokio::test]
async fn an_a_b_a_b_cycle_is_caught() {
    let cycle = (0..20).map(|i| {
        echo(
            &format!("t{i}"),
            json!({"v": if i % 2 == 1 { "b" } else { "a" }}),
        )
    });
    let p = ScriptedProvider::new(cycle);
    let (mut agent, _) = agent_with(&p, capped(|l| l.max_repeats = 3));
    collect(&mut agent, "go").await;
    assert_eq!(p.requests().len(), 5);
}

#[tokio::test]
async fn the_step_cap_stops_the_turn_with_one_nudge_on_the_way() {
    let distinct = (0..50).map(|i| echo(&format!("t{i}"), json!({"v": i.to_string()})));
    let p = ScriptedProvider::new(distinct);
    let (mut agent, _) = agent_with(&p, capped(|l| l.max_turn_steps = 6));
    let events = collect(&mut agent, "go").await;
    assert_eq!(p.requests().len(), 6);
    assert_eq!(guards(&events), 1);
}

#[test]
fn wall_time_on_the_hosts_clock_is_nudged_at_half_and_stopped_at_the_cap() {
    let clock = Arc::new(TestHooks::default());
    let mut guard = TurnGuard::new(clock.clone(), None, Remedies::default());
    let l = Some(turn_caps(|l| l.max_turn_time = Duration::from_secs(1)));
    let u = Usage::default();
    assert_eq!(guard.nudge(&u, l.as_ref()), None);
    clock.set_now(600);
    assert!(guard.nudge(&u, l.as_ref()).is_some());
    assert_eq!(guard.nudge(&u, l.as_ref()), None);
    assert_eq!(guard.stop(&u, l.as_ref()), None);
    clock.set_now(1000);
    assert!(guard.stop(&u, l.as_ref()).is_some());
}

#[tokio::test]
async fn a_model_that_answers_without_tools_is_never_nudged() {
    let p = ScriptedProvider::new([text_answer("hi")]);
    let (mut agent, _) = agent_with(&p, capped(|l| l.max_turn_steps = 1));
    let events = collect(&mut agent, "go").await;
    assert_eq!(guards(&events), 0);
    assert_eq!(end(&events).0, StopReason::EndTurn);
}

#[test]
fn a_stop_ends_with_the_hosts_remedy_for_the_cap_that_stopped_it() {
    let clock = Arc::new(TestHooks::default());
    let remedies = Remedies::new(|limit| match limit {
        Limit::TurnTime(t) if t == Duration::from_secs(1) => Some("REMEDY-TIME".into()),
        Limit::RequestTokens(10) => Some("REMEDY-REQUEST".into()),
        _ => None,
    });
    let mut guard = TurnGuard::new(clock.clone(), None, remedies);
    let l = Some(turn_caps(|l| {
        l.max_turn_time = Duration::from_secs(1);
        l.max_request_tokens = 10;
    }));
    clock.set_now(1000);
    let stop = guard.stop(&Usage::default(), l.as_ref()).unwrap();
    assert!(stop.ends_with(" REMEDY-TIME"), "{stop}");
    let refused = guard.too_big(11, l.as_ref()).unwrap();
    assert!(refused.ends_with(" REMEDY-REQUEST"), "{refused}");
}

#[test]
fn room_is_what_the_request_cap_leaves_after_the_last_request_and_the_results_since() {
    let mut guard = TurnGuard::new(Arc::new(TestHooks::default()), None, Remedies::default());
    let l = Some(turn_caps(|l| l.max_request_tokens = 1000));
    assert_eq!(guard.room(None), None);
    assert_eq!(guard.too_big(400, l.as_ref()), None);
    assert_eq!(guard.room(l.as_ref()), Some(600));
    let call = crate::messages::ToolCall {
        id: "a".into(),
        name: "Echo".into(),
        input: Default::default(),
    };
    guard.answered(&call, &ToolOutput::text("x".repeat(400)));
    assert_eq!(guard.room(l.as_ref()), Some(500));
    assert!(guard.too_big(1001, l.as_ref()).is_some());
    assert_eq!(guard.room(l.as_ref()), Some(0));
}

#[test]
fn canonical_json_ignores_key_order_at_every_depth() {
    let a: Value = json!({"b": [1, {"y": 2, "x": "s"}], "a": null});
    let b: Value = json!({"a": null, "b": [1, {"x": "s", "y": 2}]});
    let (mut ca, mut cb) = (String::new(), String::new());
    canonical(&a, &mut ca);
    canonical(&b, &mut cb);
    assert_eq!(ca, cb);
}
