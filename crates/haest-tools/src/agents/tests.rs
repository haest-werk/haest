//! Ported from host/agent-hub.test.ts (subagents). The Rust hub's own rules
//! (spend, StopAgent, refusals) are in `tests/rules.rs`.

mod lifecycle;
mod options;
mod rules;
mod script;
mod seam;
mod support;

use serde_json::json;
use tokio_util::sync::CancellationToken;

use super::{AgentError, SubagentCaps};
use support::{Rig, call, now, say, spawns};

pub(super) fn caps() -> SubagentCaps {
    SubagentCaps::default()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_foreground_agent_runs_its_answer_becomes_the_tool_result_and_the_host_sees_it_start_stream_and_stop()
 {
    let mut r = Rig::new(
        spawns(
            json!({"description": "find the bug", "prompt": "look at x"}),
            "thanks",
        ),
        now(|_, _| say("the bug is in x.ts")),
        caps(),
    );
    r.turn("go").await;
    assert!(r.texts().contains(&"the bug is in x.ts".to_string()));
    let started = r.ids("started");
    assert_eq!(started.len(), 1);
    assert!(started[0].starts_with("haest-find-the-bug-1-"));
    assert_eq!(r.ids("stopped"), started);
    assert!(r.ids("loop").contains(&started[0]));
    assert_eq!(*r.parent.activity.lock().unwrap(), [1, 0]);
}

#[tokio::test(flavor = "multi_thread")]
async fn the_same_agent_in_a_revived_host_gets_a_new_id() {
    let mut ids = Vec::new();
    for _ in 0..2 {
        let mut r = Rig::new(
            spawns(json!({"description": "scout", "prompt": "p"}), "ok"),
            now(|_, _| say("done")),
            caps(),
        );
        r.turn("go").await;
        ids.extend(r.ids("started"));
    }
    assert_eq!(ids.len(), 2);
    assert_ne!(ids[0], ids[1]);
}

#[test]
fn no_concurrent_subagents_means_no_agent_tool_to_call_and_fail() {
    let r = Rig::new(
        now(|_, _| say("x")),
        now(|_, _| say("x")),
        SubagentCaps {
            max_concurrent: 0,
            ..caps()
        },
    );
    assert!(!r.main_tools().contains(&"Agent".to_string()));
    assert!(r.main_tools().contains(&"SendMessage".to_string()));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_subagents_system_prompt_names_its_own_tools_and_model_not_mains() {
    let mut r = Rig::new(
        spawns(
            json!({"description": "x", "prompt": "p", "model": "fake:other-model"}),
            "ok",
        ),
        now(|_, _| say("done")),
        caps(),
    );
    r.turn("go").await;
    let system = r.parent.provider.sub_systems.lock().unwrap()[0].clone();
    let tools = system.lines().find(|l| l.starts_with("- Tools:")).unwrap();
    assert!(tools.contains("SendMessage"));
    assert!(!tools.contains("TaskCreate"));
    assert!(system.contains("- Model: fake:other-model"));
}

#[tokio::test(flavor = "multi_thread")]
async fn the_deepest_agent_gets_no_agent_tool_and_no_subagent_gets_the_task_list() {
    let mut r = Rig::new(
        spawns(json!({"description": "leaf", "prompt": "p"}), "ok"),
        now(|_, _| say("leaf done")),
        SubagentCaps {
            max_depth: 1,
            ..caps()
        },
    );
    r.turn("go").await;
    let tools = r.parent.provider.sub_tools.lock().unwrap()[0].clone();
    assert!(tools.contains(&"SendMessage".to_string()));
    assert!(!tools.contains(&"Agent".to_string()));
    assert!(!tools.contains(&"TaskCreate".to_string()));
}

#[tokio::test(flavor = "multi_thread")]
async fn past_the_concurrency_cap_a_spawn_is_refused_with_the_cap_named() {
    let release = CancellationToken::new();
    let gate = release.clone();
    let mut r = Rig::new(
        now(|_, n| match n {
            0 => vec![
                call(
                    "a",
                    "Agent",
                    json!({"description": "one", "prompt": "p", "run_in_background": true}),
                ),
                call(
                    "b",
                    "Agent",
                    json!({"description": "two", "prompt": "p", "run_in_background": true}),
                ),
            ],
            _ => say("ok"),
        }),
        Box::new(move |_, _| {
            let gate = gate.clone();
            Box::pin(async move {
                gate.cancelled().await;
                say("done")
            })
        }),
        SubagentCaps {
            max_concurrent: 1,
            ..caps()
        },
    );
    r.turn("go").await;
    let results = r.tool_results();
    assert!(!results[0].1);
    assert!(results[1].1);
    let refusal = AgentError::TooMany { running: 1, max: 1 };
    assert_eq!(results[1].0, refusal.to_string());
    release.cancel();
    r.settle(|r| {
        r.hub.running().is_empty()
            && r.texts()
                .iter()
                .any(|t| t.contains("[Agent \"one-1\" finished]"))
    })
    .await;
    assert!(r.hub.running().is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_background_agent_answers_later_as_a_message_and_send_message_reaches_main_while_it_runs()
{
    let mut r = Rig::new(
        spawns(
            json!({"description": "scout", "name": "scout", "prompt": "p", "run_in_background": true}),
            "noted",
        ),
        now(|_, n| match n {
            0 => vec![call(
                "s",
                "SendMessage",
                json!({"to": "main", "message": "halfway there"}),
            )],
            _ => say("scout report: all clear"),
        }),
        caps(),
    );
    r.turn("go").await;
    r.settle(|r| {
        r.texts()
            .iter()
            .any(|t| t.contains("scout report: all clear"))
    })
    .await;
    let all = r.texts();
    assert!(
        all.iter()
            .any(|t| t.contains("[Message from agent \"scout\"]") && t.contains("halfway there"))
    );
    assert!(
        all.iter()
            .any(|t| t.starts_with("[Agent \"scout\" finished]"))
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_background_answer_whose_starter_already_finished_reaches_main_instead_of_being_lost() {
    let release = CancellationToken::new();
    let gate = release.clone();
    let mut r = Rig::new(
        spawns(
            json!({"description": "mid", "name": "mid", "prompt": "p"}),
            "ok",
        ),
        Box::new(move |req, _| {
            let gate = gate.clone();
            Box::pin(async move {
                if req.system.contains("agent \"leaf\"") {
                    gate.cancelled().await;
                    return say("leaf result 42");
                }
                let spawned = req
                    .messages
                    .iter()
                    .any(|m| matches!(m, haest_core::Message::Tool(_)));
                if spawned {
                    say("mid done")
                } else {
                    vec![call(
                        "m",
                        "Agent",
                        json!({"description": "leaf", "name": "leaf", "prompt": "p", "run_in_background": true}),
                    )]
                }
            })
        }),
        caps(),
    );
    r.turn("go").await;
    release.cancel();
    r.settle(|r| {
        r.texts()
            .iter()
            .any(|t| t.starts_with("[Agent \"leaf\" finished]"))
    })
    .await;
    assert!(r.texts().iter().any(|t| t.contains("leaf result 42")));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_subagent_that_runs_past_its_step_cap_stops_and_says_so_in_its_answer() {
    let mut r = Rig::new(
        spawns(json!({"description": "looper", "prompt": "p"}), "ok"),
        now(|_, n| {
            vec![call(
                &format!("l{n}"),
                "Glob",
                json!({"pattern": format!("*.{n}")}),
            )]
        }),
        SubagentCaps { steps: 3, ..caps() },
    );
    r.turn("go").await;
    assert!(r.tool_results()[0].0.contains("stopped early"));
}
