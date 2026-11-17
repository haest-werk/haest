//! The three runs and what each proves.

use std::{path::PathBuf, sync::Arc};

use haest::{
    Agent, AgentOptions, BoxError, ShutWorkspace, StopReason, Tools, TurnOutcome, WorkspaceRef,
    core::session::{SessionRecord, SessionSink, replay},
    serde_json::json,
    tools::{AgentRef, Ending, SubagentHub},
    workspace::SandboxWorkspace,
};

use crate::{
    CWD, MODEL, T0, config, deps,
    doc::DocSink,
    gateway::{CALL_USD, Call, Gateway, reply, tool_names, tool_schema},
    helpers::{Closed, HELPER_MODEL, Node, NodeHelpers, tree_caps},
    host::{NodeSessions, NotebookHost, PAUSE_MS, RunClock},
    notebook_fs::NotebookFs,
    notebook_search, run_tools,
    tools::{Attachments, DREAM_MEMORY, DreamMaterial},
};

const NARRATION: &str = "Let me read the attachment first.";
const ANSWER: &str = "Today's note is staged for your review.";
const NOTE: &str = "# Today\nQ3 revenue up 12%.\n";
const SUB_ANSWER: &str = "Two related notes: /notebook/notes/plan.md, /notebook/notes/q2.md";

/// A work run: the sandboxed bash over NotebookFs, every notebook tool, a
/// helper on a node run of its own, a pause mid-turn, records to the node.
pub async fn work(gateway: &Arc<Gateway>) -> Result<(), BoxError> {
    let notebook = Arc::new(NotebookFs::new(&[(
        "/notebook/notes/plan.md",
        "Ship the Q3 review.\n",
    )]));
    let attachments = Arc::new(Attachments::new(&[(
        "a1",
        "# Q3 report\nRevenue up 12%.\n",
    )]));
    let sessions = Arc::new(NodeSessions::default());
    let s = config::work();
    // The owner pauses before the second model call; the notebook's clock leaves it out.
    let clock = RunClock::new(T0, true);
    let host = Arc::new(NotebookHost::new(
        clock,
        sessions.clone(),
        2,
        config::turn_caps(&s),
    ));
    let own = Tools::new().with(notebook_search());
    let base = run_tools(&s, own, attachments.clone())?;
    let workspace = WorkspaceRef::new(SandboxWorkspace::over(notebook.clone()));
    // haest's hub over the run: each helper on a node run of its own.
    let node = Arc::new(Node::default());
    let helpers = Arc::new(NodeHelpers {
        gateway: gateway.clone(),
        base: base.clone(),
        workspace: workspace.clone(),
        host: host.clone(),
        node: node.clone(),
        panel: Arc::default(),
    });
    let hub = SubagentHub::new(helpers, tree_caps());
    let mut tools = base;
    if s.agents {
        tools.extend(hub.tools_for(&AgentRef::main()));
    }
    let mut agent = Agent::new(
        deps(gateway, "", tools, workspace, host.clone()),
        AgentOptions::new(MODEL),
    );
    sessions.append(&SessionRecord::header("s-1", "c-1", CWD, T0));
    gateway.script([
        reply(
            NARRATION,
            &[
                Call("c1", "Read", json!({"file_path": "attachment:a1"})),
                Call(
                    "c2",
                    "Read",
                    json!({"file_path": "/notebook/notes/plan.md"}),
                ),
                Call(
                    "c3",
                    "Write",
                    json!({"file_path": "/notebook/notes/today.md", "content": NOTE}),
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
                Call("c5", "notebook_search", json!({"query": "revenue"})),
                Call(
                    "c6",
                    "Agent",
                    json!({"description": "Find related notes", "prompt": "Notes about Q3", "model": "haiku"}),
                ),
            ],
        ),
        // The helper's one model call, inside c6.
        reply(SUB_ANSWER, &[]),
        reply(ANSWER, &[]),
    ]);
    let mut doc = DocSink::default();
    let end = agent
        .run("Summarise attachment a1 into today's note.", &mut doc)
        .await;
    assert_eq!(
        end,
        TurnOutcome::new(StopReason::EndTurn),
        "the work run ends on its own"
    );

    // HttpClient: every model call went through the gateway; routing is the gateway's.
    let requests = gateway.take_requests();
    assert_eq!(requests.len(), 4, "main's three and the helper's one");
    for (i, r) in requests.iter().enumerate() {
        assert!(
            r.url.ends_with("/agent/llm/v1/chat/completions"),
            "{}",
            r.url
        );
        assert!(
            r.json_body().get("provider").is_none(),
            "no OpenRouter routing from the run"
        );
        // No provider key anywhere; the helper's calls carry its node run's token.
        let token = if i == 2 {
            "Bearer helper-run-1"
        } else {
            "Bearer "
        };
        assert_eq!(r.header("authorization"), Some(token));
    }
    // notebook.toml picked these; bash came without jobs: no run_in_background
    // in its schema, no job tools. Grep, Glob and WebFetch stayed off.
    let first = requests[0].json_body();
    let offered = tool_names(&first);
    assert_eq!(
        offered,
        [
            "Read",
            "Write",
            "Edit",
            "Bash",
            "notebook_search",
            "Agent",
            "SendMessage",
            "StopAgent"
        ]
    );
    assert!(
        tool_schema(&first, "Bash")["properties"]
            .get("run_in_background")
            .is_none()
    );

    // ToolCallStart: seen for every call, in order; the narration left the doc.
    assert_eq!(doc.tool_starts, ["c1", "c2", "c3", "c4", "c5", "c6"]);
    assert_eq!(doc.doc, ANSWER);

    // Read: the intercept answered attachment:a1, the fs answered the real path.
    assert!(doc.output("c1").content.contains("Revenue up 12%."));
    assert_eq!(attachments.answered(), ["attachment:a1"]);
    assert!(doc.output("c2").content.contains("Ship the Q3 review."));

    // Fs: the write is staged, and the sandboxed bash reads it from the same Fs.
    let c4 = doc.output("c4");
    assert!(
        !c4.is_error && c4.content.contains("Q3 revenue up 12%."),
        "{c4:?}"
    );
    assert_eq!(
        notebook.staged().await,
        [(PathBuf::from("/notebook/notes/today.md"), NOTE.to_string())]
    );

    // A host tool, answered by the node.
    assert!(doc.output("c5").content.contains("/notebook/notes/q2.md"));
    // The helper: the node started its run before its model call, on the model
    // the node resolved, with the notebook's explore tools only, and closed it after.
    let started = node.started();
    assert_eq!(started.len(), 1);
    assert!(started[0].name.starts_with("find-related-notes"));
    assert_eq!(started[0].asked_model.as_deref(), Some("haiku"));
    // The notebook's kinds: an Agent call naming none gets explore.
    assert_eq!(started[0].kind.as_deref(), Some("explore"));
    let helper = requests[2].json_body();
    assert_eq!(helper["model"], HELPER_MODEL.split_once(':').unwrap().1);
    assert_eq!(
        tool_names(&helper),
        ["Read", "notebook_search", "SendMessage", "StopAgent"]
    );
    let c6 = doc.output("c6");
    assert_eq!(
        (c6.content.as_str(), c6.child_cost_usd),
        (SUB_ANSWER, Some(CALL_USD))
    );
    let closed = node.closed();
    assert_eq!(
        closed,
        [Closed {
            token: started[0].token.clone(),
            ending: Ending::Answered,
            answer: SUB_ANSWER.into(),
            usd: CALL_USD,
            model_calls: 1,
            records: 2,
        }],
        "closed after its turn, its two messages kept as its run's records"
    );
    // The notebook's kinds and model text, offered to the model.
    let agent_schema = tool_schema(&first, "Agent");
    assert_eq!(
        agent_schema["properties"]["subagent_type"]["enum"],
        json!(["explore", "general"])
    );
    assert!(
        agent_schema["properties"]["model"]["description"]
            .as_str()
            .is_some_and(|d| d.contains(MODEL))
    );

    // Clock: ten paused minutes under a one-minute cap, and the guard never stirred.
    assert!(!doc.guarded());
    assert_eq!(host.clock.now(), T0);
    assert!(
        sessions.message_times().iter().all(|t| *t == T0),
        "records carry the run clock"
    );

    // Session: the node's file (with a record kind of its own) replays to the same history.
    let replayed = replay(&sessions.file_text()?);
    assert_eq!(replayed.corrupt_lines, 0);
    assert_eq!(agent.history().len(), 10);
    assert_eq!(replayed.messages, agent.history());
    Ok(())
}

/// A dream run (`config::dream`): no files, no shell, only what the node
/// hands it.
pub async fn dream(gateway: &Arc<Gateway>) -> Result<(), BoxError> {
    let s = config::dream();
    let sessions = Arc::new(NodeSessions::default());
    let host = Arc::new(NotebookHost::new(
        RunClock::new(T0, true),
        sessions,
        0,
        config::turn_caps(&s),
    ));
    let own = Tools::new().with(DreamMaterial);
    let tools = run_tools(&s, own, Arc::new(Attachments::new(&[])))?;
    let deps = deps(gateway, "", tools, ShutWorkspace, host);
    let mut agent = Agent::new(deps, AgentOptions::new(MODEL));
    gateway.script([
        reply(
            "",
            &[
                Call("d1", "dream_material", json!({})),
                Call(
                    "d2",
                    "Read",
                    json!({"file_path": "/notebook/notes/plan.md"}),
                ),
                Call("d3", "Bash", json!({"command": "ls /notebook"})),
            ],
        ),
        reply("Dreamt: one memory kept.", &[]),
    ]);
    let mut doc = DocSink::default();
    let end = agent.run("Dream.", &mut doc).await;
    assert_eq!(end, TurnOutcome::new(StopReason::EndTurn));
    let requests = gateway.take_requests();
    assert_eq!(requests.len(), 2);
    // The dream's settings took write, edit and the agents away.
    assert_eq!(
        tool_names(&requests[0].json_body()),
        ["Read", "Bash", "dream_material"]
    );
    assert!(doc.output("d1").content.contains(DREAM_MEMORY));
    assert!(doc.output("d2").is_error, "no file system in a dream");
    let d3 = doc.output("d3");
    assert!(
        d3.is_error && d3.result.as_ref().is_some_and(|r| r["exitCode"] == 127),
        "no shell in a dream"
    );
    Ok(())
}

/// The counter-proof for the clock: the same pause on a clock that counts it
/// stops the turn at its cap, before the third model call.
pub async fn clock(gateway: &Arc<Gateway>) {
    let caps = config::turn_caps(&config::work());
    assert_eq!(
        caps.max_turn_time,
        std::time::Duration::from_secs(60),
        "a work run's one-minute cap"
    );
    let host = Arc::new(NotebookHost::new(
        RunClock::new(T0, false),
        Arc::new(NodeSessions::default()),
        2,
        caps,
    ));
    let tools = Tools::new().with(notebook_search());
    let deps = deps(gateway, "", tools, ShutWorkspace, host.clone());
    let mut agent = Agent::new(deps, AgentOptions::new(MODEL));
    gateway.script([
        reply(
            "",
            &[Call("s1", "notebook_search", json!({"query": "revenue"}))],
        ),
        reply(
            "",
            &[Call("s2", "notebook_search", json!({"query": "plan"}))],
        ),
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
