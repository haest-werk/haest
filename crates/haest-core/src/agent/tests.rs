use std::sync::Arc;

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
    tool::ToolServices,
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
        Arc::new(resolve),
        Arc::new(Vec::<Arc<dyn crate::tool::Tool>>::new()),
        Arc::new(String::new()),
        "/tmp",
        Arc::new(ShutWorkspace),
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

struct Compacting;
impl ToolServices for Compacting {}
#[async_trait]
impl AgentHooks for Compacting {
    async fn before_model_call(
        &self,
        agent: &mut Agent,
        _: &CancellationToken,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let last = agent.history().last().cloned();
        agent.replace_history(last.into_iter().collect());
        Ok(())
    }
}

/// A host that cannot go on (its summary failed): the turn ends there.
struct Refusing;
impl ToolServices for Refusing {}
#[async_trait]
impl AgentHooks for Refusing {
    async fn before_model_call(
        &self,
        _: &mut Agent,
        _: &CancellationToken,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        Err("couldn't summarise the earlier conversation".into())
    }
}

#[tokio::test]
async fn the_hook_before_a_model_call_may_end_the_turn_as_an_error_without_the_call() {
    let p = ScriptedProvider::new([text_answer("never")]);
    let (mut agent, _) = agent_with(&p, Opts::default());
    agent.deps_mut().hooks = Arc::new(Refusing);
    let events = collect(&mut agent, "go").await;
    let (reason, _, error) = end(&events);
    assert_eq!(reason, StopReason::Error);
    assert_eq!(
        error.as_deref(),
        Some("couldn't summarise the earlier conversation")
    );
    assert!(p.requests().is_empty(), "no model call was made");
}

#[tokio::test]
async fn the_hook_before_each_model_call_may_replace_history() {
    let p = ScriptedProvider::new([text_answer("a")]);
    let (mut agent, _) = agent_with(&p, Opts::default());
    agent.replace_history(vec![Message::user("old"), Message::user("older")]);
    agent.deps_mut().hooks = Arc::new(Compacting);
    collect(&mut agent, "new").await;
    let sent = &p.requests()[0].messages;
    assert_eq!(sent.len(), 1);
    assert!(matches!(&sent[0], Message::User(u) if u.content == "new"));
}

/// Advertises its first tool only; the rest it keeps back, callable by name.
struct KeptBack(Vec<Arc<dyn crate::tool::Tool>>);

impl crate::tool::ToolSource for KeptBack {
    fn tools(&self) -> Vec<Arc<dyn crate::tool::Tool>> {
        self.0[..1].to_vec()
    }
    fn callable(&self) -> Vec<Arc<dyn crate::tool::Tool>> {
        self.0.clone()
    }
}

#[tokio::test]
async fn a_tool_the_source_keeps_back_runs_when_the_model_calls_it_by_name() {
    let p = ScriptedProvider::new([
        tool_answer(&[("c1", "Hidden", json!({}))]),
        text_answer("done"),
    ]);
    let (mut agent, _) = agent_with(&p, Opts::default());
    let hidden = FnTool::arc("Hidden", true, |_, _| async {
        Ok(crate::tool::ToolOutput::text("ran"))
    });
    agent.deps.tools = Arc::new(KeptBack(vec![echo_tool(), hidden]));
    collect(&mut agent, "go").await;
    let sent: Vec<String> = p.requests()[0]
        .tools
        .iter()
        .map(|t| t.name.clone())
        .collect();
    assert_eq!(sent, ["Echo"], "only the advertised tool is sent");
    assert!(
        agent
            .history()
            .iter()
            .any(|m| matches!(m, Message::Tool(r) if r.content == "ran" && !r.is_error)),
        "the kept-back tool ran: {:?}",
        agent.history()
    );
}

#[test]
fn a_turn_is_a_send_future() {
    fn assert_send<T: Send>(_: T) {}
    let p = ScriptedProvider::new([]);
    let (mut agent, _) = agent_with(&p, Opts::default());
    let mut events = Vec::new();
    assert_send(agent.run("x", &mut events));
}
