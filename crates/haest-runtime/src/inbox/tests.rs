//! The inbox keeps everything until a turn takes it, oldest first, one turn's worth at a time.

use tokio::sync::mpsc::unbounded_channel;

use super::*;
use crate::haest::{
    Agent, AgentDeps, AgentOptions, ShutWorkspace, Tool, core::testing::ScriptedProvider,
};

/// The control of an agent that never runs: what a test steers into it is
/// what the last turn left unread.
fn control() -> AgentControl {
    let none: Vec<Arc<dyn Tool>> = Vec::new();
    let deps = AgentDeps::new(
        Arc::new(ScriptedProvider::new([]).resolver()),
        Arc::new(none),
        Arc::new(String::new()),
        "/w",
        Arc::new(ShutWorkspace),
    );
    Agent::new(deps, AgentOptions::new("scripted:m")).control()
}

fn queue(cap: u32) -> (UnboundedSender<String>, InboxQueue) {
    let (tx, rx) = unbounded_channel();
    (tx, InboxQueue::new(rx, AgentTurnGate::new(cap, |_| {})))
}

#[test]
fn nothing_waiting_is_no_turn() {
    let (_tx, mut q) = queue(0);
    assert_eq!(q.take(&control()), None);
    assert_eq!(q.waiting(), 0);
}

#[test]
fn one_turn_takes_what_the_last_turn_left_unread_then_the_arrivals_oldest_first() {
    let (tx, mut q) = queue(0);
    let control = control();
    control.steer("unread");
    tx.send("first".into()).unwrap();
    tx.send("second".into()).unwrap();
    assert_eq!(q.waiting(), 2);
    let input = q.take(&control).unwrap();
    let at = |s: &str| input.find(s).unwrap();
    assert!(at("unread") < at("first") && at("first") < at("second"));
    assert_eq!(q.waiting(), 0);
    assert_eq!(q.take(&control), None, "taken once");
}

#[tokio::test]
async fn an_arrival_waited_for_is_kept_for_the_next_turn() {
    let (tx, mut q) = queue(0);
    tx.send("job exited".into()).unwrap();
    q.wait().await;
    assert_eq!(q.waiting(), 1);
    assert_eq!(q.take(&control()).as_deref(), Some("job exited"));
}

#[test]
fn held_turns_ride_in_with_the_human_and_are_never_dropped() {
    let (_tx, mut q) = queue(1);
    assert!(q.agent_turn("one").is_some());
    assert_eq!(q.agent_turn("held"), None);
    assert!(q.human("hello").contains("held"));
    assert_eq!(q.human("again"), "again");
}

#[test]
fn the_announcement_is_one_bounded_line() {
    let long = format!("\n[Agent \"x\" finished]{}\nthe answer", "y".repeat(500));
    let head = headline(&long);
    assert!(!head.contains('\n'));
    assert!(head.chars().count() < 130);
    assert!(head.starts_with("[Agent"));
    assert_eq!(headline(""), "");
}
