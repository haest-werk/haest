//! haest's own sub-agent hub over the host: a sub-agent with the host's tool
//! and command, and a background one answering through the host's inbox.

use std::time::Duration;

use super::*;

#[tokio::test]
async fn haests_own_hub_runs_a_sub_agent_with_the_hosts_tool_and_command() {
    let mut r = rig(
        vec![
            vec![call(
                "c1",
                "Agent",
                json!({"description": "Stamp the plan", "prompt": "Stamp notes/plan.md into notes/PLAN.md, then look up q3."}),
            )],
            say("The helper stamped the plan."),
        ],
        vec![
            vec![
                call(
                    "s1",
                    "Bash",
                    json!({"command": "cat notes/plan.md | stamp notes/PLAN.md"}),
                ),
                call("s2", "lookup", json!({"key": "q3"})),
            ],
            say("Stamped notes/PLAN.md; Q3 is up 12%."),
        ],
        10,
    );
    r.answered("Stamp the plan.").await;

    // Its answer is main's tool result, its cost the tool's.
    let c1 = r.output("c1");
    assert!(!c1.is_error, "{c1:?}");
    assert!(
        c1.content.contains("Stamped notes/PLAN.md; Q3 is up 12%."),
        "{}",
        c1.content
    );
    assert!(c1.child_cost_usd.is_some_and(|c| c > 0.0), "{c1:?}");

    // It ran the host's command on the host's Fs, and the host's tool.
    assert_eq!(r.file("/work/notes/PLAN.md").await, PLAN.to_uppercase());
    let subs = r.app.provider.requests(true);
    assert_eq!(subs.len(), 2);
    let Some(Message::Tool(s2)) = subs[1].messages.last() else {
        panic!("the sub-agent's second call ends on a tool result");
    };
    assert_eq!(s2.content, "up 12%");

    // The host's prompt and tools, haest's note and agent tools on top.
    assert!(
        subs[0].system.contains("You keep the books"),
        "{}",
        subs[0].system
    );
    assert!(subs[0].system.contains("`stamp <file>`"));
    for name in ["Read", "Write", "Bash", "lookup", "SendMessage"] {
        assert!(
            subs[0].tools.iter().any(|t| t == name),
            "{name} in {:?}",
            subs[0].tools
        );
    }
    let events = r.app.sub_events.lock().unwrap().clone();
    assert_eq!(events.first(), Some(&"started"));
    assert_eq!(events.last(), Some(&"stopped"));
    assert!(events.contains(&"loop"));
}

#[tokio::test]
async fn a_background_sub_agent_answers_through_the_hosts_inbox() {
    let mut r = rig(
        vec![
            vec![call(
                "c1",
                "Agent",
                json!({"description": "Count the notes", "prompt": "How many notes are there?", "run_in_background": true}),
            )],
            say("A helper is counting."),
        ],
        vec![say("One note: notes/plan.md.")],
        // The helper may not answer until main's turn has ended.
        0,
    );
    r.answered("Count the notes.").await;
    let c1 = r.output("c1");
    assert!(!c1.is_error, "{c1:?}");
    assert!(
        r.inbox.try_recv().is_err(),
        "nothing yet: the helper still works"
    );

    r.app.provider.sub_gate.add_permits(1);
    let answer = tokio::time::timeout(Duration::from_secs(5), r.inbox.recv())
        .await
        .expect("the answer arrives")
        .expect("the inbox is open");
    assert!(answer.contains("One note: notes/plan.md."), "{answer}");

    // The host runs it as main's next turn.
    r.app
        .provider
        .main
        .lock()
        .unwrap()
        .push_back(say("There is one note."));
    r.answered(&answer).await;
}
