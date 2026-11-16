use std::{collections::HashSet, sync::Arc};

use serde_json::json;

use super::*;
use crate::{
    agent::test_support::{self, Opts},
    events::LoopEvent,
    messages::{AssistantPart, ToolCall, ToolResultMessage},
    provider::Effort,
    testing::{ScriptedProvider, text_answer, tool_answer},
    tool::JsonMap,
};

mod charge;
mod summary;

fn assistant(input: u64) -> Message {
    let mut m = AssistantMessage::start("fake", "m");
    m.content.push(AssistantPart::ToolCall(ToolCall {
        id: format!("t{input}"),
        name: "X".into(),
        input: JsonMap::new(),
    }));
    m.usage.input = input;
    m.stop_reason = StopReason::ToolUse;
    Message::Assistant(m)
}

fn tool(id: &str) -> Message {
    Message::Tool(ToolResultMessage {
        tool_call_id: id.into(),
        tool_name: "X".into(),
        content: "r".into(),
        is_error: false,
        media: Vec::new(),
    })
}

fn history() -> Vec<Message> {
    vec![
        Message::user("u1"),
        assistant(1),
        tool("t1"),
        Message::user("u2"),
        assistant(2),
        tool("t2"),
        assistant(900),
        tool("t900"),
    ]
}

/// The loop tests' agent (Echo, one model), at high effort, on [`history`].
fn agent_with(provider: &Arc<ScriptedProvider>) -> Agent {
    let (mut agent, _) = test_support::agent_with(provider, Opts::default());
    agent.effort = Some(Effort::High);
    agent.messages = history();
    agent
}

fn empty_answer() -> AssistantMessage {
    AssistantMessage::start("scripted", "m")
}

/// Compact whenever asked.
const CFG: CompactionConfig = CompactionConfig {
    context_window: 1,
    threshold: 0.0,
    ceiling: None,
    keep_recent: 3,
};

/// Compact at 800 tokens.
const AT_800: CompactionConfig = CompactionConfig {
    context_window: 1000,
    threshold: 0.8,
    ceiling: None,
    keep_recent: 3,
};

#[test]
fn triggers_on_the_last_successful_request_not_the_sum() {
    assert!(needs_compaction(&history(), &AT_800));
    assert!(!needs_compaction(&history()[..6], &AT_800));
    let low_ceiling = CompactionConfig {
        ceiling: Some(2),
        ..AT_800
    };
    assert!(needs_compaction(&history()[..6], &low_ceiling));
}

#[test]
fn a_failed_request_never_triggers() {
    let mut h = history();
    if let Message::Assistant(a) = &mut h[6] {
        a.stop_reason = StopReason::Error;
    }
    assert!(!needs_compaction(&h[..7], &AT_800));
}

/// Every tool result in `tail` answers a call in `tail`, before it.
fn answered_within(tail: &[Message]) -> bool {
    let mut called = HashSet::new();
    tail.iter().all(|m| match m {
        Message::Assistant(a) => {
            called.extend(a.tool_calls().map(|c| c.id.as_str()));
            true
        }
        Message::Tool(r) => called.contains(r.tool_call_id.as_str()),
        Message::User(_) => true,
    })
}

#[test]
fn the_cut_is_the_latest_that_orphans_no_tool_result() {
    let h = history();
    for keep in 0..=h.len() + 1 {
        let cut = cut_index(&h, keep);
        assert!(cut == 0 || answered_within(&h[cut..]), "keep {keep}");
    }
    assert_eq!(
        cut_index(&h, 3),
        4,
        "before the call at 4, not its result at 5"
    );
    assert_eq!(cut_index(&h, 5), 3, "before a user message");
    assert_eq!(cut_index(&[Message::user("only")], 5), 0);
    assert_eq!(cut_index(&[], 5), 0);
}

/// The human spoke once, then only calls and their results.
fn one_long_turn(steps: u64) -> Vec<Message> {
    let mut h = vec![Message::user("go")];
    for i in 0..steps {
        h.extend([assistant(i), tool(&format!("t{i}"))]);
    }
    h
}

#[tokio::test]
async fn one_long_agentic_turn_can_be_compacted() {
    let h = one_long_turn(20);
    let cut = cut_index(&h, 12);
    assert!(cut > 0, "a turn of calls and results alone must have a cut");
    assert!(answered_within(&h[cut..]));
    let p = ScriptedProvider::new([text_answer("THE SUMMARY")]);
    let mut agent = agent_with(&p);
    agent.messages = h.clone();
    let res = compact(&mut agent, &CancellationToken::new(), &CFG).await;
    let res = res.unwrap().expect("something to compact");
    assert_eq!(agent.messages[1..], h[h.len() - res.kept..]);
    assert!(matches!(&agent.messages[0], Message::User(u) if u.content.contains("THE SUMMARY")));
    assert!(answered_within(&agent.messages));
}

#[tokio::test]
async fn compact_replaces_the_head_with_a_summary_and_keeps_the_tail_verbatim() {
    let p = ScriptedProvider::new([text_answer("THE SUMMARY")]);
    let mut agent = agent_with(&p);
    let res = compact(&mut agent, &CancellationToken::new(), &CFG)
        .await
        .unwrap()
        .unwrap();
    let cut = cut_index(&history(), CFG.keep_recent);
    assert_eq!(res.kept, history().len() - cut);
    let Message::User(first) = &agent.messages[0] else {
        panic!("summary must be a user message")
    };
    assert!(first.content.contains("THE SUMMARY"));
    assert_eq!(agent.messages[1..], history()[cut..]);
}

#[tokio::test]
async fn the_warm_summary_request_extends_the_last_loop_request() {
    let p = ScriptedProvider::new([text_answer("answer"), text_answer("THE SUMMARY")]);
    let mut agent = agent_with(&p);
    let mut events: Vec<LoopEvent> = Vec::new();
    agent.run("go", &mut events).await;
    let res = compact(&mut agent, &CancellationToken::new(), &CFG)
        .await
        .unwrap()
        .unwrap();
    let reqs = p.requests();
    let (looped, summary) = (&reqs[0], &reqs[1]);
    assert_eq!(res.path, SummaryPath::Warm);
    assert_eq!(summary.system, looped.system);
    assert_eq!(summary.tools, looped.tools);
    assert_eq!(summary.effort, looped.effort);
    assert_eq!(summary.effort, Some(Effort::High));
    assert_eq!(
        summary.messages[..looped.messages.len()],
        looped.messages[..]
    );
    assert!(matches!(summary.messages.last(), Some(Message::User(_))));
    let Message::User(first) = &agent.messages[0] else {
        panic!("summary must be a user message")
    };
    assert!(first.content.contains("THE SUMMARY"));
}

#[tokio::test]
async fn a_warm_reply_without_text_falls_back_to_a_standalone_summary_of_the_head() {
    let p = ScriptedProvider::new([
        tool_answer(&[("x", "Echo", json!({}))]),
        text_answer("COLD"),
    ]);
    let mut agent = agent_with(&p);
    let res = compact(&mut agent, &CancellationToken::new(), &CFG)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(res.path, SummaryPath::Cold);
    let reqs = p.requests();
    assert_eq!(reqs.len(), 2);
    assert!(reqs[1].tools.is_empty());
    let sent = &reqs[1].messages;
    let cut = cut_index(&history(), CFG.keep_recent);
    assert_eq!(sent[..sent.len() - 1], history()[..cut]);
    assert_eq!(res.summary, "COLD");
}

#[tokio::test]
async fn both_paths_empty_is_an_error_and_leaves_history_untouched() {
    let p = ScriptedProvider::new([empty_answer(), empty_answer()]);
    let mut agent = agent_with(&p);
    let err = compact(&mut agent, &CancellationToken::new(), &CFG).await;
    assert!(matches!(err, Err(CompactionError::Failed(..))));
    assert_eq!(agent.messages, history());
}

#[tokio::test]
async fn nothing_to_cut_sends_nothing() {
    let p = ScriptedProvider::new([]);
    let mut agent = agent_with(&p);
    agent.replace_history(vec![Message::user("only")]);
    let res = compact(&mut agent, &CancellationToken::new(), &CFG).await;
    assert!(matches!(res, Ok(None)));
    assert!(p.requests().is_empty());
}

#[test]
fn compaction_can_run_inside_a_send_hook() {
    fn assert_send<T: Send>(_: &T) {}
    let p = ScriptedProvider::new([]);
    let mut agent = agent_with(&p);
    let cancel = CancellationToken::new();
    let fut = compact(&mut agent, &cancel, &CFG);
    assert_send(&fut);
}
