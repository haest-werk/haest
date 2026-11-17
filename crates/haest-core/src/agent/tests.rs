//! The loop's turns. What a host plugs in (hooks, a tool source) is in
//! `plug_points`.

mod plug_points;

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::json;
use tokio::sync::Notify;

use super::*;
use crate::{
    agent::test_support::*,
    deps::AgentHooks,
    events::LoopEvent,
    messages::Message,
    provider::ModelError,
    testing::{ScriptedProvider, text_answer, tool_answer},
    workspace::ShutWorkspace,
};

fn roles(messages: &[Message]) -> Vec<&'static str> {
    messages
        .iter()
        .map(|m| match m {
            Message::User(_) => "user",
            Message::Assistant(_) => "assistant",
            Message::Tool(_) => "tool",
        })
        .collect()
}

fn kind(e: &LoopEvent) -> &'static str {
    match e {
        LoopEvent::User { .. } => "user",
        LoopEvent::ModelStart { .. } => "model_start",
        LoopEvent::TextDelta { .. } => "text_delta",
        LoopEvent::ThinkingDelta { .. } => "thinking_delta",
        LoopEvent::ToolCallStart { .. } => "tool_call_start",
        LoopEvent::Assistant { .. } => "assistant",
        LoopEvent::ToolResult { .. } => "tool_result",
        LoopEvent::Guard { .. } => "guard",
        LoopEvent::TurnEnd { .. } => "turn_end",
    }
}

fn results(events: &[LoopEvent]) -> Vec<&crate::messages::ToolResultMessage> {
    events
        .iter()
        .filter_map(|e| match e {
            LoopEvent::ToolResult { message, .. } => Some(message),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn a_text_only_answer_ends_the_turn_after_one_model_call() {
    let p = ScriptedProvider::new([text_answer("hi")]);
    let (mut agent, _) = agent_with(&p, Opts::default());
    let mut events = Vec::new();
    let returned = agent.run("hello", &mut events).await;
    let kinds: Vec<_> = events.iter().map(kind).collect();
    assert_eq!(
        kinds,
        ["user", "model_start", "text_delta", "assistant", "turn_end"]
    );
    let (stop, usage, _) = end(&events);
    assert_eq!(stop, StopReason::EndTurn);
    assert_eq!(usage.input, 10);
    assert_eq!(returned, TurnOutcome::new(StopReason::EndTurn));
}

#[tokio::test]
async fn tool_calls_run_results_feed_back_and_every_call_gets_exactly_one_result() {
    let p = ScriptedProvider::new([
        tool_answer(&[("a", "Echo", json!({"v": "1"})), ("b", "Nope", json!({}))]),
        text_answer("ok"),
    ]);
    let (mut agent, hooks) = agent_with(&p, Opts::default());
    let events = collect(&mut agent, "go").await;
    let results = results(&events);
    assert_eq!(results.len(), 2);
    assert_eq!(results[0].content, "echo:1");
    assert!(results[1].is_error);
    // The second request carried the tool results back to the model.
    let second = &p.requests()[1].messages;
    assert_eq!(
        second
            .iter()
            .filter(|m| matches!(m, Message::Tool(_)))
            .count(),
        2
    );
    assert_eq!(
        roles(&hooks.persisted.lock().unwrap()),
        ["user", "assistant", "tool", "tool", "assistant"]
    );
}

#[tokio::test]
async fn a_tool_call_starting_in_the_stream_is_forwarded_before_the_answer() {
    let p = ScriptedProvider::new([
        tool_answer(&[("a", "Echo", json!({"v": "1"}))]),
        text_answer("ok"),
    ]);
    let (mut agent, _) = agent_with(&p, Opts::default());
    let events = collect(&mut agent, "go").await;
    let at = |k: &str| events.iter().position(|e| kind(e) == k).unwrap();
    assert!(
        matches!(&events[at("tool_call_start")], LoopEvent::ToolCallStart { id, name } if id == "a" && name == "Echo")
    );
    assert!(at("model_start") < at("tool_call_start"));
    assert!(at("tool_call_start") < at("assistant"));
}

#[tokio::test]
async fn abort_mid_turn_still_answers_every_pending_tool_call() {
    let p = ScriptedProvider::new([tool_answer(&[
        ("a", "Echo", json!({"v": "1"})),
        ("b", "Echo", json!({"v": "2"})),
    ])]);
    let (mut agent, _) = agent_with(&p, Opts::default());
    let control = agent.control();
    let mut events = Vec::new();
    let mut sink = |ev: LoopEvent| {
        if matches!(ev, LoopEvent::Assistant { .. }) {
            control.abort();
        }
        events.push(ev);
    };
    let returned = agent.run("go", &mut sink).await;
    let results = results(&events);
    assert_eq!(results.len(), 2);
    assert!(results.iter().all(|r| r.is_error));
    assert_eq!(end(&events).0, StopReason::Aborted);
    assert_eq!(returned.stop_reason, StopReason::Aborted);
    assert!(!agent.control().running());
}

#[tokio::test]
async fn a_failed_answer_leaves_no_tool_call_without_a_result_in_history() {
    let mut failed = echo("a", json!({"v": "1"}));
    failed.stop_reason = StopReason::Error;
    failed.error_message = Some("the stream broke".into());
    let p = ScriptedProvider::new([failed, text_answer("ok")]);
    let (mut agent, hooks) = agent_with(&p, Opts::default());
    let ended = agent.run("go", &mut Vec::new()).await;
    assert_eq!(ended.stop_reason, StopReason::Error);
    let calls = |messages: &[Message]| {
        messages
            .iter()
            .filter_map(|m| match m {
                Message::Assistant(a) => Some(a.tool_calls().count()),
                _ => None,
            })
            .sum::<usize>()
    };
    assert_eq!(calls(agent.history()), 0);
    assert_eq!(calls(&hooks.persisted.lock().unwrap()), 0);
    let _ = agent.run("again", &mut Vec::new()).await;
    assert_eq!(calls(&p.requests()[1].messages), 0);
}

#[tokio::test]
async fn input_steered_mid_turn_lands_after_the_tool_results_in_the_same_turn() {
    let p = ScriptedProvider::new([
        tool_answer(&[("a", "Echo", json!({"v": "1"}))]),
        text_answer("x"),
    ]);
    let (mut agent, _) = agent_with(&p, Opts::default());
    let control = agent.control();
    let mut events = Vec::new();
    let mut sink = |ev: LoopEvent| {
        let first_answer = matches!(ev, LoopEvent::Assistant { .. })
            && !events
                .iter()
                .any(|e| matches!(e, LoopEvent::Assistant { .. }));
        if first_answer {
            control.steer("second");
        }
        events.push(ev);
    };
    let _ = agent.run("first", &mut sink).await;
    assert_eq!(
        roles(&p.requests()[1].messages),
        ["user", "assistant", "tool", "user"]
    );
    assert_eq!(count(&events, |e| matches!(e, LoopEvent::User { .. })), 2);
    assert_eq!(agent.control().take_queued(), None);
}

#[tokio::test]
async fn an_unknown_provider_ends_the_turn_as_an_error_not_a_panic() {
    let resolve = |_: &str| -> Result<crate::provider::ResolvedModel, ModelError> {
        Err(ModelError::UnknownProvider {
            provider: "zzz".into(),
            known: Vec::new(),
        })
    };
    let shared = crate::SharedDeps::new(resolve, "/tmp", ShutWorkspace);
    let deps = AgentDeps::new(shared, crate::tool::Tools::new(), "");
    let mut agent = Agent::new(deps, AgentOptions::new("zzz:m"));
    let events = collect(&mut agent, "hi").await;
    let (stop, _, error) = end(&events);
    assert_eq!(stop, StopReason::Error);
    assert!(error.unwrap().contains("zzz"));
}

#[tokio::test]
async fn a_second_turn_while_one_is_active_is_refused() {
    // `run` takes `&mut self`, so the compiler refuses a second run on the same
    // agent; the control is what a turn holds while it runs.
    let p = ScriptedProvider::new([text_answer("hi")]);
    let (mut agent, _) = agent_with(&p, Opts::default());
    let control = agent.control();
    let mut seen_running = false;
    let mut sink = |_: LoopEvent| seen_running |= control.running();
    let _ = agent.run("one", &mut sink).await;
    assert!(seen_running);
    assert!(!agent.control().running());
    let held = agent.control().begin().unwrap();
    assert!(agent.control().begin().is_err());
    drop(held);
    agent.control().end();
}

/// The calls in `messages` that no result answers in the results right after
/// their answer: what a provider refuses.
fn unanswered(messages: &[Message]) -> Vec<String> {
    let mut missing = Vec::new();
    for (i, m) in messages.iter().enumerate() {
        let Message::Assistant(a) = m else { continue };
        let answered: Vec<&str> = messages[i + 1..]
            .iter()
            .map_while(|m| match m {
                Message::Tool(r) => Some(r.tool_call_id.as_str()),
                _ => None,
            })
            .collect();
        missing.extend(
            a.tool_calls()
                .filter(|c| !answered.contains(&c.id.as_str()))
                .map(|c| c.id.clone()),
        );
    }
    missing
}

/// `Slow`: says it started, then never answers (a build longer than the host waits).
fn hanging(started: &Arc<Notify>) -> crate::tool::ToolRef {
    let started = started.clone();
    FnTool::arc("Slow", false, move |_, _| {
        let started = started.clone();
        async move {
            started.notify_one();
            std::future::pending().await
        }
    })
}

fn interrupted(m: &Message, id: &str) -> bool {
    matches!(m, Message::Tool(r)
        if r.tool_call_id == id && r.is_error && r.content == crate::session::INTERRUPTED)
}

#[tokio::test]
async fn a_turn_dropped_mid_tool_leaves_the_next_request_no_call_without_a_result() {
    let started = Arc::new(Notify::new());
    let p = ScriptedProvider::new([
        tool_answer(&[("c1", "Echo", json!({"v": "1"})), ("c2", "Slow", json!({}))]),
        text_answer("ok"),
    ]);
    let tools = vec![echo_tool(), hanging(&started)];
    let opts = Opts {
        tools: Some(tools),
        ..Default::default()
    };
    let (mut agent, hooks) = agent_with(&p, opts);
    let mut events = Vec::new();
    // The host bounds the turn the usual way and drops it while c2 runs.
    tokio::select! {
        _ = agent.run("build it", &mut events) => panic!("the turn was to be dropped mid-tool"),
        () = started.notified() => {}
    }

    let second = agent.run("never mind", &mut Vec::new()).await;

    assert_eq!(second.stop_reason, StopReason::EndTurn);
    let sent = &p.requests()[1].messages;
    assert_eq!(roles(sent), ["user", "assistant", "tool", "tool", "user"]);
    assert_eq!(unanswered(sent), Vec::<String>::new());
    assert!(interrupted(&sent[3], "c2"), "{:?}", sent[3]);
    // The answer was appended like any message: the host's file holds it.
    assert_eq!(*hooks.persisted.lock().unwrap(), agent.history());
}

#[tokio::test]
async fn a_compaction_after_a_dropped_turn_answers_its_call_first() {
    let started = Arc::new(Notify::new());
    let p = ScriptedProvider::new([
        tool_answer(&[("c1", "Echo", json!({"v": "1"})), ("c2", "Slow", json!({}))]),
        text_answer("Earlier: a, then a build."),
    ]);
    let logged = vec![
        Message::user("a"),
        Message::Assistant(text_answer("a done")),
    ];
    let opts = Opts {
        tools: Some(vec![echo_tool(), hanging(&started)]),
        history: logged.clone(),
        ..Default::default()
    };
    let (mut agent, hooks) = agent_with(&p, opts);
    let mut events = Vec::new();
    tokio::select! {
        _ = agent.run("build it", &mut events) => panic!("the turn was to be dropped mid-tool"),
        () = started.notified() => {}
    }
    // The host's /compact, between the dropped turn and the next one.
    let cfg = crate::compaction::CompactionOptions {
        keep_recent: 2,
        ..crate::compaction::CompactionOptions::new(1)
    };
    let cancel = tokio_util::sync::CancellationToken::new();

    let res = crate::compaction::compact(&mut agent, &cancel, &cfg)
        .await
        .unwrap()
        .unwrap();

    // The warm request extends history: it can carry no call without a result.
    assert_eq!(unanswered(&p.requests()[1].messages), Vec::<String>::new());
    // The answer reached the host's file before the compaction, and `kept`
    // counts the history that file replays to.
    let persisted = hooks.persisted.lock().unwrap().clone();
    assert!(
        interrupted(persisted.last().unwrap(), "c2"),
        "{persisted:?}"
    );
    let file = [logged, persisted].concat();
    assert_eq!(agent.history()[1..], file[file.len() - res.kept..]);
}

#[tokio::test]
async fn a_history_handed_in_that_ends_mid_tool_is_answered_before_the_first_request() {
    let p = ScriptedProvider::new([text_answer("ok")]);
    let call = tool_answer(&[("c1", "Echo", json!({"v": "1"})), ("c2", "Echo", json!({}))]);
    let c1 = crate::tool_exec::tool_result_message(
        &call.tool_calls().next().unwrap().clone(),
        &crate::tool::ToolOutput::text("echo:1"),
    );
    let logged = vec![
        Message::user("build it"),
        Message::Assistant(call),
        Message::Tool(c1),
    ];
    let opts = Opts {
        history: logged,
        ..Default::default()
    };
    let (mut agent, hooks) = agent_with(&p, opts);

    let end = agent.run("go on", &mut Vec::new()).await;

    assert_eq!(end.stop_reason, StopReason::EndTurn);
    let sent = &p.requests()[0].messages;
    assert_eq!(roles(sent), ["user", "assistant", "tool", "tool", "user"]);
    assert_eq!(unanswered(sent), Vec::<String>::new());
    assert!(interrupted(&sent[3], "c2"), "{:?}", sent[3]);
    // The host's file gets the answer, then the turn.
    assert_eq!(
        roles(&hooks.persisted.lock().unwrap()),
        ["tool", "user", "assistant"]
    );
}

#[tokio::test]
async fn a_history_handed_in_with_a_call_answered_by_nothing_is_answered_in_place() {
    // An older log: a turn dropped mid-tool, then another turn on top of it.
    let p = ScriptedProvider::new([text_answer("ok")]);
    let logged = vec![
        Message::user("build it"),
        Message::Assistant(tool_answer(&[("c1", "Echo", json!({}))])),
        Message::user("never mind"),
        Message::Assistant(text_answer("fine")),
    ];
    let opts = Opts {
        history: logged,
        ..Default::default()
    };
    let (mut agent, _) = agent_with(&p, opts);

    let end = agent.run("go on", &mut Vec::new()).await;

    assert_eq!(end.stop_reason, StopReason::EndTurn);
    let sent = &p.requests()[0].messages;
    assert_eq!(
        roles(sent),
        ["user", "assistant", "tool", "user", "assistant", "user"]
    );
    assert!(interrupted(&sent[2], "c1"), "{:?}", sent[2]);
}

#[test]
fn a_turn_is_a_send_future() {
    fn assert_send<T: Send>(_: T) {}
    let p = ScriptedProvider::new([]);
    let (mut agent, _) = agent_with(&p, Opts::default());
    let mut events = Vec::new();
    assert_send(agent.run("x", &mut events));
}
