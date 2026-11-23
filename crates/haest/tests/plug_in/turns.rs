//! The host's own command, tool and hook in main's turns, and the host
//! steering one.

use std::sync::atomic::{AtomicUsize, Ordering};

use haest::{AgentHooks, BoxError, CancellationToken};

use super::*;

fn exit_code(out: &ToolOutput) -> i64 {
    out.result.as_ref().unwrap()["exitCode"].as_i64().unwrap()
}

#[tokio::test]
async fn a_host_command_runs_in_the_sandbox_shell_like_a_builtin() {
    let mut r = main_rig(vec![
        vec![
            // Piped into, piped out of, chained with a builtin.
            call(
                "c1",
                "Bash",
                json!({"command": "printf 'q3,up\\nq2,flat\\n' | stamp notes/out.csv | tr a-z A-Z && cat notes/out.csv"}),
            ),
            // Relative to the shell's cwd after a cd.
            call(
                "c2",
                "Bash",
                json!({"command": "cd notes && printf 'x' | stamp here.txt"}),
            ),
            // Its own failure, as a program's.
            call("c3", "Bash", json!({"command": "stamp"})),
        ],
        say("Stamped."),
    ]);
    r.answered("Stamp the figures.").await;

    let c1 = r.output("c1");
    assert!(!c1.is_error, "{c1:?}");
    assert!(
        c1.content
            .contains("STAMPED 14 BYTES TO /WORK/NOTES/OUT.CSV"),
        "{}",
        c1.content
    );
    assert!(c1.content.contains("Q3,UP\nQ2,FLAT"), "{}", c1.content);
    assert_eq!(r.file("/work/notes/out.csv").await, "Q3,UP\nQ2,FLAT\n");

    assert!(!r.output("c2").is_error, "{:?}", r.output("c2"));
    assert_eq!(r.file("/work/notes/here.txt").await, "X");

    let c3 = r.output("c3");
    assert!(c3.is_error && exit_code(c3) == 2, "{c3:?}");
    assert!(c3.content.contains("usage: stamp <file>"), "{}", c3.content);

    // Its note reached the model with the workspace's.
    let first = &r.app.provider.requests(false)[0];
    assert!(
        first.system.contains("`stamp <file>` writes its stdin"),
        "{}",
        first.system
    );
}

#[tokio::test]
async fn a_host_tool_titles_its_call_for_the_host_and_not_for_the_model() {
    let mut r = main_rig(vec![
        vec![call("c1", "lookup", json!({"key": "q3"}))],
        say("Q3 is up 12%."),
    ]);
    r.answered("How was Q3?").await;
    let c1 = r.output("c1");
    assert_eq!(
        (c1.content.as_str(), c1.title.as_deref()),
        ("up 12%", Some("Looked up q3"))
    );

    let second = &r.app.provider.requests(false)[1];
    let Some(Message::Tool(result)) = second.messages.last() else {
        panic!("the second call ends on the tool result");
    };
    assert_eq!(
        result.content, "up 12%",
        "the title is the host's, never the model's"
    );
}

#[tokio::test]
async fn the_host_steers_a_running_turn_at_the_next_step() {
    let mut r = main_rig(vec![
        vec![call("c1", "lookup", json!({"key": "q3"}))],
        vec![call("c2", "lookup", json!({"key": "q2"}))],
        say("Q3 up 12%, Q2 flat."),
    ]);
    // The person types while lookup runs.
    *r.app.lookup.steer.lock().unwrap() = Some((r.agent.control(), "Also check Q2.".into()));
    r.answered("How was Q3?").await;

    let second = &r.app.provider.requests(false)[1];
    let tail: Vec<&Message> = second.messages.iter().rev().take(2).collect();
    assert!(
        matches!(tail[0], Message::User(u) if u.content.contains("Also check Q2.")),
        "the steer lands after the tool result: {tail:?}"
    );
    assert!(matches!(tail[1], Message::Tool(t) if t.content == "up 12%"));
    assert_eq!(r.app.provider.requests(false).len(), 3);
    assert!(
        r.agent.control().take_queued().is_none(),
        "nothing left over"
    );
}

/// A host that ends the turn before a model call: its budget is spent.
struct Budget {
    calls: AtomicUsize,
    allowed: usize,
}

#[async_trait]
impl AgentHooks for Budget {
    async fn before_model_call(
        &self,
        _agent: &mut Agent,
        _cancel: &CancellationToken,
    ) -> Result<(), BoxError> {
        if self.calls.fetch_add(1, Ordering::SeqCst) < self.allowed {
            return Ok(());
        }
        Err("The month's budget is spent.".into())
    }
}

#[tokio::test]
async fn a_hook_may_refuse_a_model_call_and_end_the_turn() {
    let mut r = main_rig(vec![
        vec![call("c1", "lookup", json!({"key": "q3"}))],
        say("never sent"),
    ]);
    let tools = r.agent.deps().tools().clone();
    let budget = Budget {
        calls: AtomicUsize::new(0),
        allowed: 1,
    };
    r.reseat(|deps| deps.with_tools(tools).with_hooks(budget));

    let end = r.turn("How was Q3?").await;
    assert_eq!(
        end,
        TurnOutcome::error(StopReason::Error, "The month's budget is spent.")
    );
    assert_eq!(
        r.app.provider.requests(false).len(),
        1,
        "no call after the refusal"
    );
    // The history stays whole: the call it made has its result.
    let kinds: Vec<&str> = r
        .agent
        .history()
        .iter()
        .map(|m| match m {
            Message::User(_) => "user",
            Message::Assistant(_) => "assistant",
            Message::Tool(_) => "tool",
            Message::ToolChange(_) => "tool_change",
        })
        .collect();
    assert_eq!(kinds, ["user", "assistant", "tool"]);
}
