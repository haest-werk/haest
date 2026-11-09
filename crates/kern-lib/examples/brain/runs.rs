//! The three runs and what each proves.

use std::{path::PathBuf, sync::Arc};

use kern::{
    Agent, AgentOptions, ShutWorkspace, StopReason, TurnEnd,
    session::{SessionRecord, SessionSink, replay},
    tool::Tool,
    tools::{BashOptions, BashTool, ReadTool, agents::AgentRef},
    workspace::SandboxWorkspace,
};
use serde_json::json;

use crate::{
    CWD, MODEL, T0,
    brain_fs::BrainFs,
    deps,
    doc::DocSink,
    gateway::{Call, Gateway, reply, tool_names, tool_schema},
    host::{BrainHost, NodeSessions, PAUSE_MS, RunClock},
    tools::{Attachments, DREAM_MEMORY, DreamMaterial, NodeAgents, SUB_ANSWER, SUB_COST_USD},
    work_tools,
};

const NARRATION: &str = "Let me read the attachment first.";
const ANSWER: &str = "Today's note is staged for your review.";
const NOTE: &str = "# Today\nQ3 revenue up 12%.\n";

/// A work run: the sandboxed bash over BrainFs, every brain tool, a pause
/// mid-turn, records to the node.
pub async fn work(gateway: &Arc<Gateway>) {
    let brain = Arc::new(BrainFs::new(&[(
        "/brain/notes/plan.md",
        "Ship the Q3 review.\n",
    )]));
    let attachments = Arc::new(Attachments::new(&[(
        "a1",
        "# Q3 report\nRevenue up 12%.\n",
    )]));
    let agents = Arc::new(NodeAgents::default());
    let sessions = Arc::new(NodeSessions::default());
    // The owner pauses before the second model call; brain's clock leaves it out.
    let host = Arc::new(BrainHost::new(RunClock::new(T0, true), sessions.clone(), 2));
    let tools = work_tools(attachments.clone(), agents.clone());
    let workspace = Arc::new(SandboxWorkspace::over(brain.clone()));
    let mut agent = Agent::new(
        deps(gateway, tools, workspace, host.clone()),
        AgentOptions::new(MODEL),
    );
    sessions.append(&SessionRecord::header("s-1", "c-1", CWD, T0));
    gateway.script([
        reply(
            NARRATION,
            &[
                Call("c1", "Read", json!({"file_path": "attachment:a1"})),
                Call("c2", "Read", json!({"file_path": "/brain/notes/plan.md"})),
                Call(
                    "c3",
                    "Write",
                    json!({"file_path": "/brain/notes/today.md", "content": NOTE}),
                ),
                Call(
                    "c4",
                    "Bash",
                    json!({"command": "cat notes/today.md", "run_in_background": true}),
                ),
            ],
        ),
        reply(
            "Now related notes.",
            &[
                Call("c5", "brain_search", json!({"query": "revenue"})),
                Call(
                    "c6",
                    "Agent",
                    json!({"description": "Find related notes", "prompt": "Notes about Q3"}),
                ),
            ],
        ),
        reply(ANSWER, &[]),
    ]);
    let mut doc = DocSink::default();
    let end = agent
        .run("Summarise attachment a1 into today's note.", &mut doc)
        .await;
    assert_eq!(
        end,
        TurnEnd::new(StopReason::EndTurn),
        "the work run ends on its own"
    );

    // Transport: every model call went through the gateway; routing is the gateway's.
    let requests = gateway.take_requests();
    assert_eq!(requests.len(), 3);
    for r in &requests {
        assert!(
            r.url.ends_with("/agent/llm/v1/chat/completions"),
            "{}",
            r.url
        );
        assert!(
            r.body.get("provider").is_none(),
            "no OpenRouter routing from the run"
        );
        assert_eq!(
            r.header("authorization"),
            Some("Bearer "),
            "the run holds no key"
        );
    }
    // Bash without jobs: no run_in_background in its schema, no job tools.
    let offered = tool_names(&requests[0].body);
    assert_eq!(
        offered,
        [
            "Read",
            "Write",
            "Edit",
            "Bash",
            "brain_search",
            "Agent",
            "SendMessage",
            "StopAgent"
        ]
    );
    assert!(
        tool_schema(&requests[0].body, "Bash")["properties"]
            .get("run_in_background")
            .is_none()
    );

    // ToolCallStart: seen for every call, in order; the narration left the doc.
    assert_eq!(doc.tool_starts, ["c1", "c2", "c3", "c4", "c5", "c6"]);
    assert_eq!(doc.doc, ANSWER);

    // Read: the intercept answered attachment:a1, the fs answered the real path.
    assert!(doc.output("c1").content.contains("Revenue up 12%."));
    assert_eq!(*attachments.answered.lock().unwrap(), ["attachment:a1"]);
    assert!(doc.output("c2").content.contains("Ship the Q3 review."));

    // Fs: the write is staged, and the sandboxed bash reads it from the same Fs.
    let c4 = doc.output("c4");
    assert!(
        !c4.is_error && c4.content.contains("Q3 revenue up 12%."),
        "{c4:?}"
    );
    assert_eq!(
        brain.staged().await,
        [(PathBuf::from("/brain/notes/today.md"), NOTE.to_string())]
    );

    // Host tool and agent tools: answered by the node.
    assert!(doc.output("c5").content.contains("/brain/notes/q2.md"));
    let c6 = doc.output("c6");
    assert_eq!(
        (c6.content.as_str(), c6.child_cost_usd),
        (SUB_ANSWER, Some(SUB_COST_USD))
    );
    let spawns = agents.spawns.lock().unwrap();
    assert_eq!(
        (&spawns[0].0, spawns[0].1.prompt.as_str()),
        (&AgentRef::main(), "Notes about Q3")
    );

    // Clock: ten paused minutes under a one-minute cap, and the guard never stirred.
    assert!(!doc.guarded());
    assert_eq!(host.clock.now(), T0);
    assert!(
        sessions.message_times().iter().all(|t| *t == T0),
        "records carry the run clock"
    );

    // Session: the node's file (with a record kind of its own) replays to the same history.
    let replayed = replay(&sessions.file_text());
    assert_eq!(replayed.corrupt_lines, 0);
    assert_eq!(agent.messages.len(), 10);
    assert_eq!(replayed.messages, agent.messages);
}

/// A dream run: no files, no shell, only what the node hands it.
pub async fn dream(gateway: &Arc<Gateway>) {
    let host = Arc::new(BrainHost::new(
        RunClock::new(T0, true),
        Arc::new(NodeSessions::default()),
        0,
    ));
    let tools: Vec<Arc<dyn Tool>> = vec![
        Arc::new(DreamMaterial),
        Arc::new(ReadTool::new(None)),
        Arc::new(BashTool::new(BashOptions::default())),
    ];
    let deps = deps(gateway, tools, Arc::new(ShutWorkspace), host);
    let mut agent = Agent::new(deps, AgentOptions::new(MODEL));
    gateway.script([
        reply(
            "",
            &[
                Call("d1", "dream_material", json!({})),
                Call("d2", "Read", json!({"file_path": "/brain/notes/plan.md"})),
                Call("d3", "Bash", json!({"command": "ls /brain"})),
            ],
        ),
        reply("Dreamt: one memory kept.", &[]),
    ]);
    let mut doc = DocSink::default();
    let end = agent.run("Dream.", &mut doc).await;
    assert_eq!(end, TurnEnd::new(StopReason::EndTurn));
    assert_eq!(gateway.take_requests().len(), 2);
    assert!(doc.output("d1").content.contains(DREAM_MEMORY));
    assert!(doc.output("d2").is_error, "no file system in a dream");
    let d3 = doc.output("d3");
    assert!(
        d3.is_error && d3.result.as_ref().unwrap()["exitCode"] == 127,
        "no shell in a dream"
    );
}

/// The counter-proof for the clock: the same pause on a clock that counts it
/// stops the turn at its cap, before the third model call.
pub async fn clock(gateway: &Arc<Gateway>) {
    let host = Arc::new(BrainHost::new(
        RunClock::new(T0, false),
        Arc::new(NodeSessions::default()),
        2,
    ));
    let tools: Vec<Arc<dyn Tool>> = vec![Arc::new(crate::brain_search())];
    let deps = deps(gateway, tools, Arc::new(ShutWorkspace), host.clone());
    let mut agent = Agent::new(deps, AgentOptions::new(MODEL));
    gateway.script([
        reply(
            "",
            &[Call("s1", "brain_search", json!({"query": "revenue"}))],
        ),
        reply("", &[Call("s2", "brain_search", json!({"query": "plan"}))]),
        reply("never sent", &[]),
    ]);
    let mut doc = DocSink::default();
    let end = agent.run("Search twice.", &mut doc).await;
    assert_eq!(
        end.stop_reason,
        StopReason::MaxTokens,
        "the time cap stopped the turn"
    );
    assert!(end.error.is_some());
    assert_eq!(
        gateway.take_requests().len(),
        2,
        "no model call after the cap"
    );
    assert_eq!(host.clock.now(), T0 + PAUSE_MS);
}
