//! The member hooks: a parent's start runs before the member's first model
//! call and may refuse it; its run names the member's model, caps and deps;
//! the run's end follows every way a member can end, with its answer and spend.

use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use serde_json::json;
use tokio_util::sync::CancellationToken;

use super::{
    caps,
    lifecycle::{Lifecycle, Step},
    support::{CALL_USD, Rig, call, now, say, spawns},
};
use crate::agents::{
    AgentDirectory, AgentError, AgentRef, Ending, MemberCaps, SpawnInput, SubagentCaps,
};

fn close(a: f64, b: f64) -> bool {
    (a - b).abs() < 1e-9
}

/// A subagent model that never answers: it waits to be cancelled.
fn hangs() -> super::support::Script {
    Box::new(|_, _| Box::pin(std::future::pending()))
}

#[tokio::test(flavor = "multi_thread")]
async fn start_runs_before_the_members_first_model_call_and_end_after_it_with_its_answer_and_spend()
{
    let l = Arc::new(Lifecycle::default());
    let mut r = Rig::with_lifecycle(
        spawns(
            json!({"description": "find it", "prompt": "look"}),
            "thanks",
        ),
        now(|_, _| say("found it")),
        caps(),
        l.clone(),
    );
    r.turn("go").await;
    let steps = l.steps();
    assert_eq!(steps.len(), 2, "{steps:?}");
    let Step::Start {
        start,
        calls_before,
    } = &steps[0]
    else {
        panic!("{steps:?}")
    };
    assert_eq!(*calls_before, 0, "started before its first model call");
    assert_eq!(
        (start.model_spec.as_str(), start.input.prompt.as_str()),
        ("fake:main-model", "look")
    );
    assert_eq!(start.caps, MemberCaps::from(caps()));
    let Step::End {
        name,
        end,
        calls_before,
    } = &steps[1]
    else {
        panic!("{steps:?}")
    };
    assert_eq!(name, &start.name);
    assert_eq!(*calls_before, 1, "ended after its model call");
    assert_eq!(end.ending, Ending::Answered);
    assert_eq!(end.answer, "found it");
    assert_eq!(end.spend.model_calls, 1);
    assert!(close(end.spend.usd(), CALL_USD));
    // The answer the end heard is the one the caller got.
    assert_eq!(r.tool_results()[0], (end.answer.clone(), false));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_refused_start_refuses_the_spawn_as_the_parents_typed_error_runs_nothing_and_frees_the_name()
 {
    let l = Arc::new(Lifecycle {
        refuse: Some("deploy"),
        ..Lifecycle::default()
    });
    let mut r = Rig::with_lifecycle(
        spawns(
            json!({"description": "deploy it", "name": "d", "prompt": "p"}),
            "ok",
        ),
        now(|_, _| say("deployed")),
        caps(),
        l.clone(),
    );
    r.turn("go").await;
    let refusal = AgentError::Refused("no deploy here".into());
    // The model reads the parent's refusal as the Agent call's input error.
    assert_eq!(r.tool_results()[0], (refusal.to_string(), true));
    // A host calling the hub gets the parent's own error back.
    let ask = |description: &str| SpawnInput {
        description: description.into(),
        prompt: "p".into(),
        name: Some("d".into()),
        ..Default::default()
    };
    let main = AgentRef::main();
    let again = r
        .hub
        .spawn(&main, ask("deploy now"), CancellationToken::new());
    assert_eq!(again.await, Err(refusal));
    assert!(r.provider_sub_tools().is_empty(), "nothing ran");
    assert!(r.hub.running().is_empty());
    assert!(
        l.ends().is_empty(),
        "no end for a member that never started"
    );
    // The name never ran, so it is free again.
    let out = r.hub.spawn(&main, ask("look"), CancellationToken::new());
    assert_eq!(out.await.unwrap().text, "deployed");
    let names: Vec<String> = (l.steps().into_iter())
        .filter_map(|s| match s {
            Step::Start { start, .. } => Some(start.name),
            Step::End { .. } => None,
        })
        .collect();
    assert_eq!(names, ["d", "d", "d"]);
}

#[tokio::test(flavor = "multi_thread")]
async fn stop_agent_reaches_the_end_and_a_background_answer_goes_back_only_after_it() {
    let l = Arc::new(Lifecycle::default());
    let mut r = Rig::with_lifecycle(
        spawns(
            json!({"description": "slow", "name": "slow", "prompt": "p", "run_in_background": true}),
            "ok",
        ),
        hangs(),
        caps(),
        l.clone(),
    );
    r.turn("go").await;
    assert!(r.hub.stop("slow"));
    let finished = |t: &String| t.starts_with("[Agent \"slow\" finished]");
    r.settle(|r| r.texts().iter().any(finished)).await;
    let answer = r.texts().into_iter().find(finished).unwrap();
    let ends = l.ends();
    assert_eq!(
        ends.len(),
        1,
        "the end ran before the answer was handed back"
    );
    assert_eq!(ends[0].ending, Ending::Stopped);
    assert!(answer.ends_with(&ends[0].answer), "{answer}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_member_stopped_while_its_run_starts_never_runs_and_its_end_says_stopped() {
    let wait = Arc::new(tokio::sync::Notify::new());
    let l = Arc::new(Lifecycle {
        wait: Some(wait.clone()),
        ..Lifecycle::default()
    });
    let r = Rig::with_lifecycle(
        now(|_, _| say("x")),
        now(|_, _| say("ran")),
        caps(),
        l.clone(),
    );
    let hub = r.hub.clone();
    let input = SpawnInput {
        description: "late".into(),
        prompt: "p".into(),
        name: Some("late".into()),
        background: true,
        ..Default::default()
    };
    let spawn = tokio::spawn(async move {
        hub.spawn(&AgentRef::main(), input, CancellationToken::new())
            .await
    });
    let until = Instant::now() + Duration::from_secs(5);
    while l.steps().is_empty() && Instant::now() < until {
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    // Starting counts as running: StopAgent reaches it.
    assert_eq!(r.hub.running(), ["late"]);
    assert!(r.hub.stop("late"));
    wait.notify_one();
    spawn.await.unwrap().unwrap();
    while r.hub.answers_due() > 0 && Instant::now() < until {
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    assert!(r.provider_sub_tools().is_empty(), "no model call");
    let ends = l.ends();
    assert_eq!(ends.len(), 1);
    assert_eq!(ends[0].ending, Ending::Stopped);
    assert_eq!(ends[0].spend.model_calls, 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_cancelled_caller_reaches_its_foreground_members_end() {
    let l = Arc::new(Lifecycle::default());
    let r = Rig::with_lifecycle(now(|_, _| say("x")), hangs(), caps(), l.clone());
    let cancel = CancellationToken::new();
    let (hub, caller) = (r.hub.clone(), cancel.clone());
    let input = SpawnInput {
        description: "wait".into(),
        prompt: "p".into(),
        ..Default::default()
    };
    let spawn = tokio::spawn(async move { hub.spawn(&AgentRef::main(), input, caller).await });
    let end = Instant::now() + Duration::from_secs(5);
    while r.provider_sub_tools().is_empty() && Instant::now() < end {
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    cancel.cancel();
    let out = spawn.await.unwrap().unwrap();
    let ends = l.ends();
    assert_eq!(ends.len(), 1);
    assert_eq!(ends[0].ending, Ending::CallerCancelled);
    assert_eq!(ends[0].answer, out.text);
}

#[tokio::test(flavor = "multi_thread")]
async fn the_run_a_start_returns_names_the_members_model_caps_and_deps() {
    let l = Arc::new(Lifecycle {
        model_spec: Some("fake:node-model".into()),
        caps: Some(MemberCaps {
            usd: 1.0,
            steps: 1,
            time: Duration::from_secs(60),
        }),
        own_deps: true,
        ..Lifecycle::default()
    });
    let mut r = Rig::with_lifecycle(
        spawns(json!({"description": "loop", "prompt": "p"}), "ok"),
        now(|_, n| {
            vec![call(
                &format!("l{n}"),
                "Glob",
                json!({"pattern": format!("*.{n}")}),
            )]
        }),
        SubagentCaps {
            steps: 50,
            ..caps()
        },
        l.clone(),
    );
    r.turn("go").await;
    // Its model: the run's, not the call's or main's.
    let system = r.parent.provider.sub_systems.lock().unwrap()[0].clone();
    assert!(system.contains("- Model: fake:node-model"), "{system}");
    // Its caps: one model call, then the run's step cap stopped it.
    assert_eq!(r.provider_sub_tools().len(), 1);
    assert!(matches!(l.ends()[0].ending, Ending::Capped(_)));
    // Its deps: their hooks kept its messages, their ledger took its spend.
    assert!(!l.kept.messages.lock().unwrap().is_empty());
    let own = l.kept.ledger.lock().unwrap().clone().unwrap();
    assert!(close(own.lock().unwrap().usd(), CALL_USD));
    // Main's two calls on the conversation's ledger, the member's on its own.
    assert!(close(r.parent.ledger.lock().unwrap().usd(), 2.0 * CALL_USD));
}
