//! Tool changes that keep the cache: what a change is, where the loop puts
//! it, and the baseline across a failed call, a model switch, a resume, a
//! compaction, a pre-warm and a second agent.

use std::{sync::Arc, time::Duration};

use serde_json::json;
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

use super::*;
use crate::{
    agent::test_support::{FnTool, Opts, TestHooks, agent_with, collect, roles},
    caps::estimate_request_tokens,
    catalog::{CacheFacts, ModelFacts},
    compaction::{CompactionOptions, compact},
    messages::StopReason,
    provider::ProviderRequest,
    session::{SessionRecord, replay},
    testing::{ScriptedProvider, text_answer, tool_answer},
    tool::{LiveTools, ToolOutput, ToolOverride, ToolRef, Tools},
};

/// A tool called `name` whose description is its name, answering "ok".
fn tool(name: &str) -> ToolRef {
    FnTool::arc(name, false, |_, _| async { Ok(ToolOutput::text("ok")) })
}

/// `tool(name)` as the model sees it.
fn spec(name: &str) -> ToolSpec {
    ToolSpec {
        name: name.into(),
        description: name.into(),
        parameters: json!({"type": "object"}),
    }
}

/// `tool(name)` with another description: a new definition of it.
fn redefined(name: &str, description: &str) -> ToolRef {
    ToolRef::new(ToolOverride::new(tool(name)).with_description(description))
}

fn specs(names: &[&str]) -> Vec<ToolSpec> {
    names.iter().map(|n| spec(n)).collect()
}

fn removed(name: &str) -> ToolChange {
    ToolChange::Removed { name: name.into() }
}

fn restored(name: &str) -> ToolChange {
    ToolChange::Restored { name: name.into() }
}

fn added(tool: ToolSpec) -> ToolChange {
    ToolChange::Added { tool }
}

/// Every model takes tool changes but the `plain:` ones.
struct Takes;

impl ModelFacts for Takes {
    fn cache_facts(&self, spec: &str) -> Option<CacheFacts> {
        Some(CacheFacts::new().with_tool_changes(!spec.starts_with("plain:")))
    }
}

/// An agent on `fake:m1`, a model that takes tool changes, served by `p`,
/// its tools `live`.
fn agent_on(p: &ScriptedProvider, live: &LiveTools) -> (Agent, Arc<TestHooks>) {
    let (mut agent, hooks) = agent_with(p, Opts::default());
    agent.map_deps(|d| d.with_tools(live.clone()).with_facts(Takes));
    (agent, hooks)
}

/// One turn, failing the test rather than waiting forever.
async fn turn(agent: &mut Agent, input: &str) {
    tokio::time::timeout(Duration::from_secs(10), collect(agent, input))
        .await
        .expect("the turn ended");
}

/// The tool changes `messages` holds, in order.
fn changes_in(messages: &[Message]) -> Vec<&ToolChangeMessage> {
    let changes = messages.iter().filter_map(|m| match m {
        Message::ToolChange(c) => Some(c),
        _ => None,
    });
    changes.collect()
}

/// The `tools` array a provider that renders tool changes sends, as bytes.
fn sent_tool_bytes(req: &ProviderRequest) -> Vec<u8> {
    serde_json::to_vec(req.sent_tools()).unwrap()
}

#[test]
fn a_tool_put_in_goes_by_its_definition_and_one_taken_out_by_its_name() {
    let base = specs(&["A", "B"]);
    let change = tool_change(Some(&base), &specs(&["A", "C"]), &[Message::user("hi")]);
    let change = change.expect("a change");
    assert_eq!(change.changes, [removed("B"), added(spec("C"))]);
    assert_eq!(
        change.baseline,
        Some(base),
        "the first carries the baseline"
    );
}

#[test]
fn a_new_definition_under_a_name_is_put_in_by_value_and_replaces_it() {
    let base = specs(&["A"]);
    let mut v2 = spec("A");
    v2.description = "A, version 2".into();
    let change = tool_change(
        Some(&base),
        std::slice::from_ref(&v2),
        &[Message::user("hi")],
    );
    assert_eq!(change.unwrap().changes, [added(v2)]);
}

#[test]
fn a_tool_taken_out_and_put_back_comes_back_by_name_or_by_its_new_definition() {
    let base = specs(&["A", "B"]);
    let hidden = ToolChangeMessage {
        changes: vec![removed("B")],
        baseline: Some(base.clone()),
    };
    let history = |last: Message| {
        vec![
            Message::user("one"),
            Message::ToolChange(hidden.clone()),
            last,
        ]
    };
    let back = tool_change(Some(&base), &base, &history(Message::user("two")));
    assert_eq!(back.unwrap().changes, [restored("B")]);
    // Back with another definition: by value, the baseline's would be wrong.
    let mut v2 = spec("B");
    v2.description = "B, version 2".into();
    let current = [spec("A"), v2.clone()];
    let back = tool_change(Some(&base), &current, &history(Message::user("two")));
    let back = back.unwrap();
    assert_eq!(back.changes, [added(v2)]);
    assert_eq!(back.baseline, None, "only the first carries the baseline");
}

#[test]
fn a_tool_put_in_by_value_then_taken_out_comes_back_by_value() {
    let base = specs(&["A"]);
    let history = [
        Message::user("one"),
        Message::ToolChange(ToolChangeMessage {
            changes: vec![added(spec("C"))],
            baseline: Some(base.clone()),
        }),
        Message::user("two"),
        Message::ToolChange(ToolChangeMessage {
            changes: vec![removed("C")],
            baseline: None,
        }),
        Message::user("three"),
    ];
    let back = tool_change(Some(&base), &specs(&["A", "C"]), &history);
    assert_eq!(
        back.unwrap().changes,
        [added(spec("C"))],
        "C is not in the tools array"
    );
}

#[test]
fn no_change_and_no_model_that_takes_one_record_nothing() {
    let base = specs(&["A"]);
    let hi = [Message::user("hi")];
    assert_eq!(tool_change(Some(&base), &base, &hi), None);
    assert_eq!(tool_change(None, &specs(&["B"]), &hi), None);
}

#[test]
fn a_change_goes_after_a_user_or_tool_message_and_waits_after_an_answer() {
    let base = specs(&["A"]);
    let now = specs(&["A", "B"]);
    let call = tool_answer(&[("c1", "A", json!({}))]);
    let result = Message::Tool(crate::session::interrupted(
        call.tool_calls().next().unwrap().clone(),
    ));
    let after_results = [Message::user("go"), Message::Assistant(call), result];
    assert!(tool_change(Some(&base), &now, &after_results).is_some());
    let after_answer = [Message::user("go"), Message::Assistant(text_answer("done"))];
    assert_eq!(tool_change(Some(&base), &now, &after_answer), None, "waits");
    assert_eq!(tool_change(Some(&base), &now, &[]), None, "never first");
}

#[test]
fn the_request_estimate_counts_the_baseline_once_as_the_tools() {
    let base = specs(&["A", "B", "C"]);
    let with = |baseline| {
        let change = ToolChangeMessage {
            changes: vec![removed("C")],
            baseline,
        };
        [Message::user("hi"), Message::ToolChange(change)]
    };
    assert_eq!(
        estimate_request_tokens("sys", &with(Some(base.clone())), &base),
        estimate_request_tokens("sys", &with(None), &base)
    );
}

#[test]
fn an_empty_answer_says_nothing_and_a_signed_thought_or_a_call_does() {
    let mut empty = AssistantMessage::start("anthropic", "m");
    assert!(!says_something(&empty));
    empty.content.push(AssistantPart::Text {
        text: String::new(),
    });
    assert!(!says_something(&empty));
    let mut thought = AssistantMessage::start("anthropic", "m");
    thought.content.push(AssistantPart::Thinking {
        thinking: "hm".into(),
        signature: Some("sig".into()),
        redacted: None,
    });
    assert!(says_something(&thought));
    assert!(says_something(&tool_answer(&[("c", "A", json!({}))])));
}

#[tokio::test]
async fn a_change_between_turns_waits_for_the_next_user_message_and_rides_its_call() {
    let p = ScriptedProvider::new([text_answer("one"), text_answer("two"), text_answer("three")]);
    let live = LiveTools::from(Tools::from(vec![tool("A"), tool("B")]));
    let (mut agent, hooks) = agent_on(&p, &live);
    turn(&mut agent, "one").await;
    // Between turns: two edits, one call.
    live.set_tool(tool("C"));
    assert!(live.set_enabled("B", false));
    turn(&mut agent, "two").await;
    turn(&mut agent, "three").await;

    let sent = p.requests();
    assert_eq!(sent[0].tool_baseline, Some(specs(&["A", "B"])));
    assert_eq!(sent[1].tool_baseline, Some(specs(&["A", "B"])));
    assert_eq!(
        sent[1].tools,
        specs(&["A", "C"]),
        "the current tools stay there"
    );
    let change = ToolChangeMessage {
        changes: vec![removed("B"), added(spec("C"))],
        baseline: Some(specs(&["A", "B"])),
    };
    assert_eq!(
        roles(&sent[1].messages),
        ["user", "assistant", "user", "tool_change"]
    );
    assert_eq!(sent[1].messages[3], Message::ToolChange(change.clone()));
    let history = roles(agent.history());
    assert_eq!(
        history,
        [
            "user",
            "assistant",
            "user",
            "tool_change",
            "assistant",
            "user",
            "assistant"
        ]
    );
    // Recorded once, persisted where it sits, and sent again as it is.
    assert_eq!(changes_in(agent.history()), [&change]);
    assert_eq!(roles(&hooks.persisted.lock().unwrap()), history);
    assert_eq!(sent[2].messages[..4], sent[1].messages[..]);
    assert_eq!(sent_tool_bytes(&sent[0]), sent_tool_bytes(&sent[2]));
}

/// A tool called `Slow` that says it started and waits for `go`.
fn slow(started: &Arc<Notify>, go: &Arc<Notify>) -> ToolRef {
    let (started, go) = (started.clone(), go.clone());
    FnTool::arc("Slow", false, move |_, _| {
        let (started, go) = (started.clone(), go.clone());
        async move {
            started.notify_one();
            go.notified().await;
            Ok(ToolOutput::text("ok"))
        }
    })
}

#[tokio::test]
async fn a_change_during_a_turn_goes_after_the_tool_results_never_between_a_call_and_its_result() {
    let (started, go) = (Arc::new(Notify::new()), Arc::new(Notify::new()));
    let live = LiveTools::from(Tools::new().with(slow(&started, &go)));
    let p = ScriptedProvider::new([
        tool_answer(&[("c1", "Slow", json!({}))]),
        text_answer("done"),
    ]);
    let (mut agent, _) = agent_on(&p, &live);
    let host = async {
        started.notified().await;
        live.set_tool(tool("Late"));
        go.notify_one();
    };
    let ((), ()) = tokio::join!(turn(&mut agent, "go"), host);

    let sent = p.requests();
    assert_eq!(
        roles(&sent[1].messages),
        ["user", "assistant", "tool", "tool_change"]
    );
    assert_eq!(
        roles(agent.history()),
        ["user", "assistant", "tool", "tool_change", "assistant"]
    );
    assert_eq!(
        changes_in(agent.history())[0].changes,
        [added(spec("Late"))]
    );
}

#[tokio::test]
async fn a_failed_call_records_no_change_and_the_next_call_sends_it_again() {
    let mut failed = AssistantMessage::start("scripted", "m");
    failed.stop_reason = StopReason::Error;
    failed.error_message = Some("overloaded".into());
    let p = ScriptedProvider::new([text_answer("one"), failed, text_answer("three")]);
    let live = LiveTools::from(Tools::new().with(tool("A")));
    let (mut agent, _) = agent_on(&p, &live);
    turn(&mut agent, "one").await;
    live.set_tool(tool("B"));
    turn(&mut agent, "two").await;
    assert!(changes_in(agent.history()).is_empty(), "nothing recorded");
    turn(&mut agent, "three").await;

    let sent = p.requests();
    let change = Message::ToolChange(ToolChangeMessage {
        changes: vec![added(spec("B"))],
        baseline: Some(specs(&["A"])),
    });
    assert_eq!(sent[1].messages.last(), Some(&change));
    assert_eq!(
        sent[2].messages.last(),
        Some(&change),
        "after the newest user message"
    );
    assert_eq!(changes_in(agent.history()).len(), 1);
}

#[tokio::test]
async fn an_answer_with_nothing_in_it_records_no_change() {
    let p = ScriptedProvider::new([text_answer("one"), AssistantMessage::start("scripted", "m")]);
    let live = LiveTools::from(Tools::new().with(tool("A")));
    let (mut agent, _) = agent_on(&p, &live);
    turn(&mut agent, "one").await;
    live.set_tool(tool("B"));
    turn(&mut agent, "two").await;
    assert!(changes_in(agent.history()).is_empty());
}

#[tokio::test]
async fn a_model_that_takes_no_tool_changes_gets_the_current_tools_as_before() {
    let p = ScriptedProvider::new([text_answer("one"), text_answer("two")]);
    let live = LiveTools::from(Tools::new().with(tool("A")));
    let (mut agent, _) = agent_on(&p, &live);
    agent.set_model_spec("plain:m1");
    turn(&mut agent, "one").await;
    live.set_tool(tool("B"));
    turn(&mut agent, "two").await;

    let sent = p.requests();
    assert_eq!(sent[1].tool_baseline, None);
    assert_eq!(sent[1].tools, specs(&["A", "B"]));
    assert!(changes_in(agent.history()).is_empty());
}

#[tokio::test]
async fn a_model_switch_keeps_both_ways_right() {
    let p = ScriptedProvider::new((0..5).map(|i| text_answer(&format!("a{i}"))));
    let live = LiveTools::from(Tools::new().with(tool("A")));
    let (mut agent, _) = agent_on(&p, &live);
    // No change yet: a switch to a model that takes them starts its baseline there.
    agent.set_model_spec("plain:m1");
    turn(&mut agent, "0").await;
    live.set_tool(tool("B"));
    agent.set_model_spec("fake:m1");
    turn(&mut agent, "1").await;
    // A change on it, then away: the other model gets the current tools,
    // the change left in history for its provider to leave out.
    live.set_tool(tool("C"));
    turn(&mut agent, "2").await;
    agent.set_model_spec("plain:m1");
    live.set_tool(tool("D"));
    turn(&mut agent, "3").await;
    // And back: the baseline in history goes on, its change sent as it was.
    agent.set_model_spec("fake:m1");
    turn(&mut agent, "4").await;

    let sent = p.requests();
    assert_eq!(
        sent[1].tool_baseline,
        Some(specs(&["A", "B"])),
        "started there"
    );
    assert!(changes_in(&sent[1].messages).is_empty());
    assert_eq!(sent[2].tool_baseline, Some(specs(&["A", "B"])));
    assert_eq!(sent[3].tool_baseline, None);
    assert_eq!(sent[3].tools, specs(&["A", "B", "C", "D"]));
    assert_eq!(sent[4].tool_baseline, Some(specs(&["A", "B"])));
    let changes: Vec<_> = changes_in(&sent[4].messages)
        .into_iter()
        .map(|c| c.changes.clone())
        .collect();
    assert_eq!(changes, [vec![added(spec("C"))], vec![added(spec("D"))]]);
    assert_eq!(
        sent[4].messages[..sent[2].messages.len()],
        sent[2].messages[..]
    );
}

#[tokio::test]
async fn a_resumed_conversation_sends_the_same_request_again() {
    let p = ScriptedProvider::new([text_answer("one"), text_answer("two")]);
    let live = LiveTools::from(Tools::new().with(tool("A")));
    let (mut agent, hooks) = agent_on(&p, &live);
    turn(&mut agent, "one").await;
    live.set_tool(redefined("A", "A, version 2"));
    turn(&mut agent, "two").await;
    // The session file the persistence hook would write.
    let file: Vec<String> = (hooks.persisted.lock().unwrap().iter())
        .map(|m| {
            let record = SessionRecord::Message {
                t: 1,
                message: m.clone(),
            };
            serde_json::to_string(&record).unwrap()
        })
        .collect();
    let replayed = replay(&file.join("\n"));
    assert_eq!(replayed.messages, agent.history());

    let p2 = ScriptedProvider::new([text_answer("three")]);
    let (mut resumed, _) = agent_with(
        &p2,
        Opts {
            history: replayed.messages,
            ..Opts::default()
        },
    );
    resumed.map_deps(|d| d.with_tools(live.clone()).with_facts(Takes));
    turn(&mut resumed, "three").await;

    let (before, after) = (&p.requests()[1], &p2.requests()[0]);
    assert_eq!(sent_tool_bytes(before), sent_tool_bytes(after));
    assert_eq!(after.messages[..before.messages.len()], before.messages[..]);
    assert_eq!(
        changes_in(resumed.history()).len(),
        1,
        "nothing new to record"
    );
}

#[tokio::test]
async fn a_compaction_drops_the_changes_and_starts_the_baseline_over() {
    let p = ScriptedProvider::new([
        text_answer("one"),
        text_answer("two"),
        text_answer("THE SUMMARY"),
        text_answer("three"),
    ]);
    let live = LiveTools::from(Tools::new().with(tool("A")));
    let (mut agent, _) = agent_on(&p, &live);
    turn(&mut agent, "one").await;
    live.set_tool(tool("B"));
    turn(&mut agent, "two").await;
    let cfg = CompactionOptions {
        keep_recent: 3,
        ..CompactionOptions::new(1)
    };
    let done = compact(&mut agent, &CancellationToken::new(), &cfg).await;
    let done = done.unwrap().expect("compacted");
    assert!(changes_in(agent.history()).is_empty());
    live.set_tool(tool("C"));
    turn(&mut agent, "three").await;

    let sent = p.requests();
    // The summary request was the last loop request plus its instruction.
    assert_eq!(sent[2].tool_baseline, sent[1].tool_baseline);
    assert_eq!(
        sent[2].messages[..sent[1].messages.len()],
        sent[1].messages[..]
    );
    // After it, the current tools are the baseline.
    assert_eq!(sent[3].tool_baseline, Some(specs(&["A", "B", "C"])));
    assert!(changes_in(&sent[3].messages).is_empty());
    // A replay of the same compaction drops the same changes.
    let mut log: Vec<String> = Vec::new();
    for m in &sent[1].messages {
        log.push(
            serde_json::to_string(&SessionRecord::Message {
                t: 1,
                message: m.clone(),
            })
            .unwrap(),
        );
    }
    log.push(
        serde_json::to_string(&SessionRecord::Message {
            t: 1,
            message: Message::Assistant(text_answer("two")),
        })
        .unwrap(),
    );
    let record = SessionRecord::Compaction {
        t: 2,
        summary: done.summary,
        kept: done.kept,
    };
    log.push(serde_json::to_string(&record).unwrap());
    assert_eq!(
        replay(&log.join("\n")).messages,
        agent.history()[..agent.history().len() - 2]
    );
}

#[tokio::test]
async fn a_pre_warm_sends_the_last_request_again_with_its_baseline() {
    let mut ping = AssistantMessage::start("scripted", "m");
    ping.usage.cache_read = 100;
    let p = ScriptedProvider::new([text_answer("one"), text_answer("two"), ping]);
    let live = LiveTools::from(Tools::new().with(tool("A")));
    let (mut agent, _) = agent_on(&p, &live);
    turn(&mut agent, "one").await;
    live.set_tool(tool("B"));
    turn(&mut agent, "two").await;
    // A change still waiting for the next user message is not in it.
    live.set_tool(tool("C"));
    agent.warm_cache(&CancellationToken::new()).await.unwrap();

    let sent = p.requests();
    assert!(sent[2].prewarm);
    assert_eq!(sent_tool_bytes(&sent[2]), sent_tool_bytes(&sent[1]));
    assert_eq!(sent[2].messages, sent[1].messages);
}

#[tokio::test]
async fn each_agent_keeps_its_own_baseline() {
    let live = LiveTools::from(Tools::new().with(tool("A")));
    let p = ScriptedProvider::new([
        text_answer("main 1"),
        text_answer("sub 1"),
        text_answer("main 2"),
    ]);
    let (mut main, _) = agent_on(&p, &live);
    turn(&mut main, "one").await;
    live.set_tool(tool("B"));
    // A subagent started now, on the same tools, starts from them.
    let (mut sub, _) = agent_on(&p, &live);
    turn(&mut sub, "task").await;
    turn(&mut main, "two").await;

    let sent = p.requests();
    assert_eq!(sent[1].tool_baseline, Some(specs(&["A", "B"])));
    assert!(changes_in(sub.history()).is_empty());
    assert_eq!(sent[2].tool_baseline, Some(specs(&["A"])));
    assert_eq!(changes_in(main.history())[0].changes, [added(spec("B"))]);
}
