//! The inbox keeps everything until a turn takes it, oldest first, one turn's worth at a time.

use haest_core::{
    Agent, AgentDeps, AgentOptions, SharedDeps, ShutWorkspace, Tools, testing::ScriptedProvider,
};
use tokio::sync::mpsc::unbounded_channel;

use super::*;

/// The control of an agent that never runs: what a test steers into it is
/// what the last turn left unread.
fn control() -> AgentControl {
    let shared = SharedDeps::new(ScriptedProvider::new([]).resolver(), "/w", ShutWorkspace);
    let deps = AgentDeps::new(shared, Tools::new(), "");
    Agent::new(deps, AgentOptions::new("scripted:m")).control()
}

fn queue(cap: u32) -> (UnboundedSender<String>, InboxQueue) {
    let (tx, rx) = unbounded_channel();
    (tx, InboxQueue::new(rx, AgentTurnGate::new(cap, |_| {})))
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
    assert!(q.wait().await);
    assert_eq!(q.waiting(), 1);
    assert!(q.wait().await, "one already waits: at once");
    assert_eq!(q.take(&control()).as_deref(), Some("job exited"));
}

#[tokio::test]
async fn waiting_ends_false_once_nothing_can_arrive() {
    let (tx, mut q) = queue(0);
    tx.send("last words".into()).unwrap();
    drop(tx);
    assert!(q.wait().await, "what was sent still arrives");
    assert_eq!(q.take(&control()).as_deref(), Some("last words"));
    assert!(!q.wait().await);
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
