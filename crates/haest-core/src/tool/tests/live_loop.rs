//! What the loop makes of a [`LiveTools`] that changes while a conversation
//! runs: the request a replacement leaves, a call to a tool taken out, a call
//! already running when its tool changes, and the next model call seeing a
//! change.

use std::{
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use tokio::sync::Notify;

use super::{
    live::{answering, names},
    *,
};
use crate::{
    agent::{
        Agent,
        test_support::{FnTool, Opts, agent_with, collect, echo_tool, end},
    },
    events::LoopEvent,
    messages::{Message, StopReason},
    provider::ProviderRequest,
    testing::{ScriptedProvider, text_answer, tool_answer},
    tool_exec::{ToolIndex, run_unspilled},
    workspace::ShutWorkspace,
};

/// An agent served by `p` whose tools are `live`.
fn agent_on(p: &ScriptedProvider, live: &LiveTools) -> Agent {
    let (mut agent, _) = agent_with(p, Opts::default());
    agent.map_deps(|d| d.with_tools(live.clone()));
    agent
}

/// One turn, failing the test rather than waiting forever.
async fn turn(agent: &mut Agent, input: &str) -> Vec<LoopEvent> {
    tokio::time::timeout(Duration::from_secs(10), collect(agent, input))
        .await
        .expect("the turn ended")
}

/// The request's tool part as bytes: every provider builds the tool list it
/// sends from these specs alone.
fn tool_bytes(req: &ProviderRequest) -> Vec<u8> {
    serde_json::to_vec(&req.tools).unwrap()
}

/// What every tool call of the turn produced, in call order.
fn results(events: &[LoopEvent]) -> Vec<ToolOutput> {
    let answers = events.iter().filter_map(|e| match e {
        LoopEvent::ToolResult { output, .. } => Some(output.clone()),
        _ => None,
    });
    answers.collect()
}

#[tokio::test]
async fn an_identical_replacement_leaves_the_request_byte_for_byte_as_it_was() {
    let p = ScriptedProvider::new([
        tool_answer(&[("c1", "Look", json!({}))]),
        text_answer("one"),
        tool_answer(&[("c2", "Look", json!({}))]),
        text_answer("two"),
    ]);
    let live = LiveTools::from(Tools::from(vec![
        answering("First", "-"),
        answering("Look", "old"),
        answering("Last", "-"),
    ]));
    let mut agent = agent_on(&p, &live);
    let before = turn(&mut agent, "one").await;
    // A new instance: the same name, description and schema.
    live.set_tool(answering("Look", "new"));
    let after = turn(&mut agent, "two").await;

    let sent = p.requests();
    assert_eq!(
        tool_bytes(&sent[0]),
        tool_bytes(&sent[2]),
        "the cached prefix holds"
    );
    assert_eq!(sent[0].system, sent[2].system);
    let ran = [&results(&before)[0].content, &results(&after)[0].content];
    assert_eq!(ran, ["old", "new"], "the second turn ran the new tool");
}

/// A tool called `Gone` that counts its runs, beside Echo; `take_out` takes it out.
async fn call_a_tool_taken_out(take_out: fn(&LiveTools)) {
    let runs = Arc::new(AtomicUsize::new(0));
    let counted = runs.clone();
    let gone = FnTool::arc("Gone", false, move |_, _| {
        counted.fetch_add(1, Ordering::SeqCst);
        async { Ok(ToolOutput::text("ran")) }
    });
    let echo = echo_tool();
    let live = LiveTools::from(Tools::from(vec![echo.clone(), gone]));
    take_out(&live);
    let p = ScriptedProvider::new([tool_answer(&[("c1", "Gone", json!({}))]), text_answer("ok")]);
    let mut agent = agent_on(&p, &live);
    let events = turn(&mut agent, "go").await;

    let sent = p.requests();
    assert_eq!(sent[0].tools.len(), 1, "only Echo is advertised");
    assert_eq!(sent[0].tools[0].name, "Echo");
    assert_eq!(runs.load(Ordering::SeqCst), 0, "the tool never ran");
    // The loop's answer to a name its tools do not hold, as it gives it.
    let call = ToolCall {
        id: "c1".into(),
        name: "Gone".into(),
        input: JsonMap::new(),
    };
    let ctx = ToolContext::new("/", ShutWorkspace);
    let unknown = run_unspilled(&ToolIndex::new(Tools::new().with(echo)), &call, &ctx).await;
    let answered = results(&events);
    assert_eq!(answered.len(), 1);
    assert!(answered[0].is_error);
    assert_eq!(answered[0], unknown);
    // The model read it, and the turn went on to its answer.
    assert!(
        sent[1]
            .messages
            .iter()
            .any(|m| matches!(m, Message::Tool(r) if r.is_error && r.content == unknown.content))
    );
    assert_eq!(end(&events).0, StopReason::EndTurn);
}

#[tokio::test]
async fn a_disabled_tool_is_not_advertised_and_a_call_to_it_is_an_unknown_tool() {
    call_a_tool_taken_out(|live| assert!(live.set_enabled("Gone", false))).await;
}

#[tokio::test]
async fn a_removed_tool_is_not_advertised_and_a_call_to_it_is_an_unknown_tool() {
    call_a_tool_taken_out(|live| assert!(live.remove_tool("Gone"))).await;
}

/// A tool called `name` that says it started, waits for `go`, then answers `text`.
fn waiting(name: &str, text: &'static str, started: &Arc<Notify>, go: &Arc<Notify>) -> ToolRef {
    let (started, go) = (started.clone(), go.clone());
    FnTool::arc(name, false, move |_, _| {
        let (started, go) = (started.clone(), go.clone());
        async move {
            started.notify_one();
            go.notified().await;
            Ok(ToolOutput::text(text))
        }
    })
}

#[tokio::test]
async fn a_call_already_running_finishes_on_the_tool_it_started_with() {
    for remove in [false, true] {
        let (started, go) = (Arc::new(Notify::new()), Arc::new(Notify::new()));
        let live = LiveTools::from(Tools::new().with(waiting("Slow", "v1", &started, &go)));
        let p = ScriptedProvider::new([
            tool_answer(&[("c1", "Slow", json!({}))]),
            tool_answer(&[("c2", "Slow", json!({}))]),
            text_answer("done"),
        ]);
        let mut agent = agent_on(&p, &live);
        // The host changes the tool while its call runs.
        let host = async {
            started.notified().await;
            if remove {
                live.remove_tool("Slow");
            } else {
                live.set_tool(answering("Slow", "v2"));
            }
            go.notify_one();
        };
        let (events, ()) = tokio::join!(turn(&mut agent, "go"), host);

        let answered = results(&events);
        assert_eq!(answered[0], ToolOutput::text("v1"), "remove: {remove}");
        // The next model call reads the set as the host left it.
        if remove {
            assert!(p.requests()[1].tools.is_empty());
            assert!(answered[1].is_error, "gone for the next call");
        } else {
            assert_eq!(answered[1], ToolOutput::text("v2"));
        }
    }
}

#[tokio::test]
async fn a_change_made_during_a_turn_is_in_its_next_model_call() {
    let (started, go) = (Arc::new(Notify::new()), Arc::new(Notify::new()));
    let live = LiveTools::from(Tools::new().with(waiting("Slow", "ok", &started, &go)));
    let p = ScriptedProvider::new([
        tool_answer(&[("c1", "Slow", json!({}))]),
        tool_answer(&[("c2", "Late", json!({}))]),
        text_answer("done"),
    ]);
    let mut agent = agent_on(&p, &live);
    // The host's own clone: the agent holds another.
    let hosts = live.clone();
    let host = async {
        started.notified().await;
        hosts.set_tool(answering("Late", "late"));
        go.notify_one();
    };
    let (events, ()) = tokio::join!(turn(&mut agent, "go"), host);

    let sent = p.requests();
    let advertised = |i: usize| {
        sent[i]
            .tools
            .iter()
            .map(|t| t.name.clone())
            .collect::<Vec<_>>()
    };
    assert_eq!(advertised(0), ["Slow"]);
    assert_eq!(advertised(1), ["Slow", "Late"]);
    assert_eq!(names(&live.tools()), ["Slow", "Late"]);
    assert_eq!(results(&events)[1], ToolOutput::text("late"));
}
