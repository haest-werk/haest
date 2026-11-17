//! BashOutput / KillShell / Bash's background switch, and the conversation half
//! ported from host/jobs.test.ts: a background job's exit wakes the model.

use std::{sync::Arc, time::Duration};

use haest_core::{
    Agent, AgentDeps, AgentOptions, LoopEvent, Message, ToolRef, Tools,
    testing::{ScriptedProvider, text_answer, tool_answer},
    tool::{Tool, ToolError, ToolOutput},
};
use serde_json::{Value, json};

use super::job_tools;
use crate::{
    BashOptions, BashTool,
    jobs::{
        JobStatus, MAX_JOBS,
        tests::{Rig, until},
    },
    testkit::input,
};

fn bash(r: &Rig) -> BashTool {
    BashTool::new(BashOptions::default().with_jobs(r.hub.clone()))
}

async fn call(tool: &dyn Tool, r: &Rig, v: Value) -> Result<ToolOutput, ToolError> {
    tool.execute(input(v), &r.ctx).await
}

#[tokio::test(flavor = "multi_thread")]
async fn a_background_command_returns_at_once_and_the_job_tools_read_and_stop_it() {
    let r = Rig::real();
    let [output, kill] = <[ToolRef; 2]>::try_from(Vec::from_iter(job_tools(r.hub.clone())))
        .ok()
        .unwrap();
    let started = call(
        &bash(&r),
        &r,
        // The output (42) is not in the command, which BashOutput names.
        json!({"command": "echo $((40 + 2)); sleep 30", "run_in_background": "true"}),
    )
    .await
    .unwrap();
    assert!(!started.is_error);
    let id = r.hub.running()[0].id.clone();
    assert!(started.content.contains(&id));

    until(3000, || r.hub.buffer(&id).unwrap().contains("42")).await;
    let first = call(output.as_ref(), &r, json!({"bash_id": id}))
        .await
        .unwrap();
    assert!(first.content.contains("42"));
    let again = call(output.as_ref(), &r, json!({"bash_id": id}))
        .await
        .unwrap();
    assert!(!again.content.contains("42"), "only what is new");

    call(kill.as_ref(), &r, json!({"shell_id": id}))
        .await
        .unwrap();
    assert_eq!(r.hub.get(&id).unwrap().status, JobStatus::Killed);
    let twice = call(kill.as_ref(), &r, json!({"shell_id": id}))
        .await
        .unwrap();
    assert!(!twice.is_error);

    let unknown = call(output.as_ref(), &r, json!({"bash_id": "b9"})).await;
    assert!(matches!(unknown, Err(ToolError::Input(m)) if m.contains("b9")));
    assert!(matches!(
        call(kill.as_ref(), &r, json!({})).await,
        Err(ToolError::Input(_))
    ));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_background_job_past_the_cap_is_refused_for_the_model_to_act_on() {
    let r = Rig::real();
    let tool = bash(&r);
    for _ in 0..MAX_JOBS {
        let v = json!({"command": "sleep 30", "run_in_background": true});
        call(&tool, &r, v).await.unwrap();
    }
    let v = json!({"command": "sleep 30", "run_in_background": true});
    assert!(matches!(
        call(&tool, &r, v).await,
        Err(ToolError::Failed(_))
    ));
    r.hub.kill_all();
}

fn user_texts(messages: &[Message]) -> Vec<String> {
    messages
        .iter()
        .filter_map(|m| match m {
            Message::User(u) => Some(u.content.clone()),
            _ => None,
        })
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_background_job_shows_in_the_running_set_and_its_exit_wakes_the_model_with_a_new_turn() {
    let mut r = Rig::real();
    let provider = ScriptedProvider::new([
        tool_answer(&[(
            "a",
            "Bash",
            json!({"command": "sleep 0.1; echo built", "run_in_background": true}),
        )]),
        text_answer("started"),
        text_answer("noted the exit"),
    ]);
    let mut tools: Tools = Tools::from_iter([ToolRef::new(bash(&r))]);
    tools.extend(job_tools(r.hub.clone()));
    let deps = AgentDeps::new(
        Arc::new(provider.resolver()),
        Arc::new(tools),
        Arc::new("system".to_string()),
        r.dir.clone(),
        r.ctx.workspace().clone(),
    );
    let mut agent = Agent::new(deps, AgentOptions::new("scripted:m"));
    let mut events: Vec<LoopEvent> = Vec::new();
    agent.run("start the build", &mut events).await;

    // The host's inbox: the exit arrives as a turn the human did not start.
    let notice = tokio::time::timeout(Duration::from_secs(5), r.notices.recv())
        .await
        .expect("the exit is announced")
        .unwrap();
    agent.run(&notice, &mut events).await;

    let snapshots = r.events.snapshots.lock().unwrap().clone();
    assert_eq!(snapshots.first(), Some(&vec!["b1".to_string()]));
    assert_eq!(snapshots.last(), Some(&Vec::new()));
    let chunks = r.events.chunks.lock().unwrap().clone();
    let printed: String = chunks.iter().map(|(_, c, _)| c.as_str()).collect();
    assert!(printed.contains("built"));
    assert!(user_texts(agent.history()).last().unwrap().contains("b1"));
    assert!(matches!(
        agent.history().last(),
        Some(Message::Assistant(_))
    ));
}
