//! A panel run's helper in the background: the person writes to it while it
//! works and the panel hears it, its answer waits for main's next turn as an
//! answer (not a message), and the run is settled once that answer has been
//! handed back.
//!
//! PLUG POINTS: `SubagentHub::deliver` from `Sender::Person`,
//! `SubagentEvent::Message`, `Inbox::submit_answer`, `SubagentHub::settled`,
//! `SubagentParent::subagent_note`.

use std::time::Duration;

use haest::{
    BoxError, ShutWorkspace, ToolContext, Tools,
    core::tool,
    serde_json::json,
    tools::{AgentDirectory, AgentMessage, AgentRef, Ending, Sender, SubagentHub},
};

use crate::{
    CWD, T0, config,
    gateway::{Gateway, reply},
    helpers::{ForMain, Node, NodeHelpers, Panel, tree_caps},
    host::{NodeSessions, NotebookHost, RunClock},
    notebook_search, run_tools,
    tools::Attachments,
};

/// What the person types to the helper.
const ASKED: &str = "Only Q3, skip Q2.";
const FOUND: &str = "Q3 only: /notebook/notes/plan.md";

pub(crate) async fn panel(gateway: &Gateway) -> Result<(), BoxError> {
    let s = config::work();
    let sessions = NodeSessions::default();
    let host = NotebookHost::new(RunClock::new(T0, true), sessions, 0, config::turn_caps(&s));
    let own = Tools::new().with(notebook_search());
    let base = run_tools(&s, own, Attachments::new(&[]))?;
    let (node, panel) = (Node::default(), Panel::default());
    let helpers = NodeHelpers {
        gateway: gateway.clone(),
        base,
        workspace: ShutWorkspace.into(),
        host,
        node: node.clone(),
        panel: panel.clone(),
    };
    let hub = SubagentHub::new(helpers, tree_caps());

    // The helper's first model call is slow: the person writes meanwhile.
    let held = gateway.hold("Bearer helper-run-1");
    gateway.script([reply("Looking.", &[]), reply(FOUND, &[])]);
    // Main's Agent call, in the background.
    let agent = hub.tools_for(&AgentRef::main())[0].clone();
    let call = tool::input(
        json!({"description": "Find the Q3 notes", "prompt": "Every note about Q3.", "run_in_background": true}),
    )?;
    let ctx = ToolContext::new(CWD, ShutWorkspace);
    let started = agent.execute(call, &ctx).await?.content;
    assert!(started.starts_with("Started agent"), "{started}");
    held.arrived.cancelled().await;
    let name = node.started()[0].name.clone();

    // The person types to the helper from the panel, and the panel shows it
    // as theirs.
    let typed = AgentMessage::new(Sender::Person("owner".into()), name.clone(), ASKED);
    let said = hub.deliver(typed.clone())?;
    assert_eq!(
        said,
        format!("Delivered to \"{name}\"; it reads it at its next step.")
    );
    assert_eq!(panel.messages(), [typed]);
    assert!(
        !hub.settled(Duration::from_millis(20)).await,
        "the helper still works"
    );

    held.release.cancel();
    assert!(hub.settled(Duration::from_secs(5)).await);
    // Its answer waits for main's next turn: an answer, never a message.
    let answer = format!("[Agent \"{name}\" finished]\n\n{FOUND}");
    assert_eq!(panel.main(), [ForMain::Answer(answer)]);
    let closed = node.closed();
    assert_eq!(closed.len(), 1);
    assert_eq!(closed[0].ending, Ending::Answered);

    // It read the person's words at its next step, under who wrote them.
    let requests = gateway.take_requests();
    assert_eq!(
        requests.len(),
        2,
        "its slow call, then one after the message"
    );
    let second = requests[1].json_body().to_string();
    assert!(second.contains("[Message from owner, the person you work for]"));
    assert!(second.contains(ASKED));
    // The notebook's own note, not haest's: it names the person, and explore only reads.
    let first = requests[0].json_body().to_string();
    assert!(first.contains("# You are a sub-agent"));
    assert!(first.contains("(or from the person)"));
    assert!(first.contains("You only read and search"));
    assert!(!first.contains("# You are a subagent"), "haest's note");
    Ok(())
}
