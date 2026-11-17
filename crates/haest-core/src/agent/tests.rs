//! The loop's turns. What a host plugs in (hooks, a tool source) is in
//! `plug_points`.

mod plug_points;

use async_trait::async_trait;
use serde_json::json;

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
    agent.run("again", &mut Vec::new()).await;
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
    agent.run("first", &mut sink).await;
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
    let deps = AgentDeps::new(
        resolve,
        crate::tool::Tools::new(),
        "",
        "/tmp",
        ShutWorkspace,
    );
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
    agent.run("one", &mut sink).await;
    assert!(seen_running);
    assert!(!agent.control().running());
    let held = agent.control().begin().unwrap();
    assert!(agent.control().begin().is_err());
    drop(held);
    agent.control().end();
}

#[test]
fn a_turn_is_a_send_future() {
    fn assert_send<T: Send>(_: T) {}
    let p = ScriptedProvider::new([]);
    let (mut agent, _) = agent_with(&p, Opts::default());
    let mut events = Vec::new();
    assert_send(agent.run("x", &mut events));
}
