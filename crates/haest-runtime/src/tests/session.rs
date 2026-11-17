//! The whole conversation behind a session: the inbox's turns go through its
//! gate, a person's turn takes what the gate held, and closing the session
//! shuts down everything the conversation started.

use std::{path::Path, sync::Arc, time::Duration};

use async_trait::async_trait;
use haest_session::{Body, Frame, Hub, Input, SessionPrompter, Subscription, start};
use haest_settings::Settings;
use haest_wire::{Answer, Level, Outbound, State};
use rmcp::{
    ErrorData, RoleServer, ServerHandler, ServiceExt,
    model::{
        InitializeResult, ListToolsResult, PaginatedRequestParams, ServerCapabilities,
        Tool as McpTool,
    },
    service::RequestContext,
};
use serde_json::json;
use tokio_util::sync::CancellationToken;

use super::{Gateway, hardcoded, openrouter_key};
use crate::{
    BuildOptions, HubUi, Runtime, RuntimeInputs, World, build,
    haest::{
        LoopEvent,
        core::fs::{Fs, MemFs},
        core::session::SessionRecord,
        core::tool::ToolContext,
        http::{HttpClient, HttpClientRef, HttpError, Request, Response},
        mods::{ApprovalRequest, Decision, ModUiRef},
        tools::{AgentRef, Inbox, JobHub, NoInbox, SpawnInput},
        workspace::RealWorkspace,
    },
    inbox::inbox,
    testkit::Deny,
};

/// A host's client whose requests never answer: a model call waits until it
/// is stopped.
struct Hang;

#[async_trait]
impl HttpClient for Hang {
    async fn send(&self, _: Request) -> Result<Response, HttpError> {
        std::future::pending().await
    }
}

/// A conversation over `fs` whose UI is the hub's, its model calls sent to `http`.
async fn built(
    fs: &Arc<MemFs>,
    settings: Settings,
    http: impl Into<HttpClientRef>,
    hub: &Hub,
) -> Runtime<HubUi> {
    let inputs = RuntimeInputs::new(settings, World::files(fs.clone()))
        .with_keys(openrouter_key())
        .with_http(http);
    build(
        inputs,
        HubUi::new(hub.clone()),
        Deny,
        BuildOptions::default(),
    )
    .await
    .unwrap()
}

/// An MCP server in this process with one tool; the handle ends when its
/// client leaves.
#[derive(Clone)]
struct Echo;

impl ServerHandler for Echo {
    fn get_info(&self) -> InitializeResult {
        InitializeResult::new(ServerCapabilities::builder().enable_tools().build())
    }

    async fn list_tools(
        &self,
        _: Option<PaginatedRequestParams>,
        _: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        let schema = json!({"type": "object", "properties": {}});
        let schema = Arc::new(schema.as_object().unwrap().clone());
        Ok(ListToolsResult::with_all_items(vec![McpTool::new(
            "echo", "Echo", schema,
        )]))
    }
}

/// Frames until (and with) the first `stop` picks; panics after 10 s.
async fn until(sub: &mut Subscription, stop: impl Fn(&Frame) -> bool) -> Vec<Arc<Frame>> {
    let mut seen = Vec::new();
    let read = async {
        while let Some(frame) = sub.recv().await {
            let done = stop(&frame);
            seen.push(frame);
            if done {
                return;
            }
        }
    };
    tokio::time::timeout(Duration::from_secs(10), read)
        .await
        .expect("the frame never came");
    seen
}

fn turn_end(f: &Frame) -> bool {
    f.path.is_main() && matches!(f.body, Body::Loop(LoopEvent::TurnEnd { .. }))
}

/// The text of every user message the conversation's own turns took.
fn users(frames: &[Arc<Frame>]) -> Vec<String> {
    (frames.iter())
        .filter(|f| f.path.is_main())
        .filter_map(|f| match &f.body {
            Body::Loop(LoopEvent::User { message }) => Some(message.content.clone()),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn close_stops_the_subagents_kills_the_jobs_closes_mcp_and_flushes() {
    let fs = Arc::new(MemFs::new());
    let hub = Hub::new("s1");
    let settings = Settings {
        agents: true,
        ..hardcoded()
    };
    let mut rt = built(&fs, settings, Hang, &hub).await;

    // A background subagent waiting on its model call.
    let spawn = SpawnInput {
        description: "wait on the model".into(),
        prompt: "go".into(),
        background: true,
        ..SpawnInput::default()
    };
    (rt.subagents()
        .spawn(&AgentRef::main(), spawn, &CancellationToken::new())
        .await)
        .unwrap();
    assert_eq!(rt.subagents().running().len(), 1);
    // A background job.
    let jobs = JobHub::new(NoInbox);
    let ws = RealWorkspace::new(std::env::vars().collect());
    let ctx = ToolContext::new(std::env::temp_dir(), ws);
    jobs.start(haest_tools::NewJob::new("sleep 30"), &ctx)
        .unwrap();
    rt.jobs = Some(jobs.clone());
    // An MCP server.
    let (client, server) = tokio::io::duplex(64 * 1024);
    let served = tokio::spawn(async move {
        if let Ok(running) = Echo.serve(server).await {
            let _ = running.waiting().await;
        }
    });
    rt.mcp().connect_transport("echo", client).await;
    assert_eq!(rt.mcp().tools().len(), 1);
    // A record the session file has not written yet.
    rt.hooks().append(&SessionRecord::Config {
        t: 1,
        model_spec: Some("openrouter:held/back".into()),
        effort: None,
    });

    let (agents, mcp, file) = (rt.subagents().clone(), rt.mcp().clone(), rt.session_id());
    let session = start(rt, hub);
    session.close().await;

    assert!(agents.running().is_empty(), "the subagent stopped");
    assert!(jobs.running().is_empty(), "the job was killed");
    assert!(mcp.tools().is_empty(), "the MCP server was closed");
    tokio::time::timeout(Duration::from_secs(10), served)
        .await
        .expect("the server saw its client leave")
        .unwrap();
    let path = Path::new("/data/sessions").join(format!("{file}.jsonl"));
    let saved = fs.read_to_string(&path).await.unwrap();
    assert!(
        saved.contains("openrouter:held/back"),
        "the session file was flushed"
    );
}

#[tokio::test]
async fn an_arrival_runs_through_the_gate_and_what_it_holds_rides_in_with_the_person() {
    let fs = Arc::new(MemFs::new());
    let hub = Hub::new("s1");
    let mut rt = built(&fs, hardcoded(), Gateway::default(), &hub).await;
    // An inbox the test can send to, its gate letting one turn through.
    let (sender, queue) = inbox(rt.hooks(), 1);
    *rt.inbox_mut() = queue;
    let mut sub = hub.subscribe(0);
    let session = start(rt, hub);

    sender.submit("job b1 exited".into());
    let first = users(&until(&mut sub, turn_end).await);
    assert_eq!(first.len(), 1, "it ran as a turn of its own: {first:?}");
    assert!(
        first[0].ends_with("job b1 exited"),
        "through the gate, which may add a note: {first:?}"
    );

    sender.submit("job b2 exited".into());
    let held = until(&mut sub, |f| {
        matches!(
            &f.body,
            Body::Wire(Outbound::Notice {
                level: Level::Warning,
                ..
            })
        )
    })
    .await;
    assert!(
        users(&held).is_empty(),
        "the gate held the second: {held:?}"
    );
    session.idle().await;

    session.turn(Input::human("what happened?")).await.unwrap();
    let mine = until(&mut sub, turn_end).await;
    let text = users(&mine).concat();
    assert!(
        text.contains("job b2 exited") && text.contains("what happened?"),
        "{text}"
    );
    session.close().await;
}

#[tokio::test]
async fn the_status_names_the_session_file_and_follows_a_model_change() {
    let fs = Arc::new(MemFs::new());
    let hub = Hub::new("s1");
    let rt = built(&fs, hardcoded(), Gateway::default(), &hub).await;
    let id = rt.session_id();
    let session = start(rt, hub);

    let now = session.snapshot().await.unwrap();
    assert_eq!(
        (now.state, now.session.as_str()),
        (State::Idle, id.as_str())
    );
    let other = "openrouter:openai/gpt-5.5";
    session
        .control(haest_session::Control::SetModelSpec(other.into()))
        .await
        .unwrap();
    assert_eq!(session.snapshot().await.unwrap().model_spec, other);
    let bad = session
        .control(haest_session::Control::SetModelSpec("nowhere:x".into()))
        .await;
    assert!(bad.is_err());
    assert_eq!(
        session.snapshot().await.unwrap().model_spec,
        other,
        "a bad spec changes nothing"
    );

    session
        .control(haest_session::Control::Clear)
        .await
        .unwrap();
    assert_ne!(
        session.snapshot().await.unwrap().session,
        id,
        "a fresh session file"
    );
    session.close().await;
}

#[tokio::test]
async fn hub_ui_lines_are_notices_in_the_stream() {
    let hub = Hub::new("s1");
    let mut sub = hub.subscribe(0);
    let ui = HubUi::new(hub.clone());
    crate::Ui::warn(&ui, "an MCP server did not start");
    crate::Ui::arrival(&ui, "job b1 exited");
    let frames = until(&mut sub, |f| f.seq == 2).await;
    let notices: Vec<(Level, String)> = (frames.iter())
        .filter_map(|f| match &f.body {
            Body::Wire(Outbound::Notice { level, text }) => Some((*level, text.clone())),
            _ => None,
        })
        .collect();
    assert_eq!(
        notices,
        [
            (Level::Warning, "an MCP server did not start".to_string()),
            (Level::Info, "job b1 exited".to_string()),
        ]
    );
}

/// The next approval ask in the stream: its number.
async fn next_ask(sub: &mut Subscription) -> u64 {
    let frames = until(sub, |f| {
        matches!(f.body, Body::Wire(Outbound::ApprovalAsk { .. }))
    })
    .await;
    match &frames.last().unwrap().body {
        Body::Wire(Outbound::ApprovalAsk { ask, .. }) => *ask,
        _ => unreachable!(),
    }
}

/// Asks put to whoever drives the session are the runtime's gate's: an
/// ALWAYS answered over the session covers the next ask, and the host takes
/// it back on [`Runtime::gate`](crate::Runtime).
#[tokio::test]
async fn an_always_answered_over_the_session_is_the_runtimes_gate() {
    let hub = Hub::new("s1");
    let inputs =
        RuntimeInputs::new(hardcoded(), World::files(MemFs::new())).with_keys(openrouter_key());
    let ui = HubUi::new(hub.clone());
    let prompter = SessionPrompter::new(hub.clone());
    let rt = build(inputs, ui, prompter, BuildOptions::default())
        .await
        .unwrap();
    let (asks, gate) = (rt.mods().env().ui().clone(), rt.gate().clone());
    let mut sub = hub.subscribe(0);
    let session = start(rt, hub);
    let rm = || ApprovalRequest {
        tool_name: "Bash".into(),
        description: "haest wants to delete files".into(),
        input_preview: "rm -rf build".into(),
        tool_use_id: None,
        key: Some("rm -rf build".into()),
    };
    let ask = |asks: &ModUiRef| {
        let asks = asks.clone();
        tokio::spawn(async move { asks.ask(rm(), &CancellationToken::new()).await })
    };

    let first = ask(&asks);
    let id = next_ask(&mut sub).await;
    session.answer(id, Answer::Always).unwrap();
    assert_eq!(first.await.unwrap(), Decision::Allow);
    let covered = asks.ask(rm(), &CancellationToken::new()).await;
    assert_eq!(covered, Decision::Allow, "the ALWAYS covers it, no ask");

    gate.revoke("Bash");
    let again = ask(&asks);
    let id = next_ask(&mut sub).await;
    session.answer(id, Answer::Deny).unwrap();
    assert_eq!(
        again.await.unwrap(),
        Decision::Deny,
        "asked over the session again"
    );
    session.close().await;
}
