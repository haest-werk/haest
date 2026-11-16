//! Mods in a conversation: the decorators around a real agent loop, the way a
//! host composes them.

use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

use haest_core::{
    Agent, AgentDeps, AgentOptions, Message, ToolResultMessage,
    deps::SystemPrompt,
    testing::{ScriptedProvider, text_answer, tool_answer},
    tool::{Tool, ToolSource},
    workspace::{ShutWorkspace, WorkspaceMode},
};
use serde_json::json;

use crate::{
    ApprovalAnswer, ApprovalGate, ApprovalMod, CommandOutcome, ModEnv, ModSystemPrompt, ModTools,
    Mods, PLAN_PROMPT, PlanMod, testkit::*,
};

fn agent(p: &Arc<ScriptedProvider>, mods: &Arc<Mods>, tools: Vec<Arc<dyn Tool>>) -> Agent {
    let source: Arc<dyn ToolSource> = Arc::new(tools);
    let base: Arc<dyn SystemPrompt> = Arc::new("You are haest.".to_string());
    let deps = AgentDeps::new(
        Arc::new(p.resolver()),
        Arc::new(ModTools::new(source, mods.clone())),
        Arc::new(ModSystemPrompt::new(base, mods.clone())),
        CWD,
        Arc::new(ShutWorkspace),
    );
    Agent::new(deps, AgentOptions::new("scripted:m"))
}

fn tool_results(agent: &Agent) -> Vec<&ToolResultMessage> {
    agent
        .history()
        .iter()
        .filter_map(|m| match m {
            Message::Tool(t) => Some(t),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn an_irreversible_command_waits_for_the_human_deny_refuses_and_always_lets_the_next_through()
{
    let human = Human::answering([ApprovalAnswer::Deny, ApprovalAnswer::AllowAlways]);
    // A prompt past those two answers is a deny soon, not a hang.
    let gate = ApprovalGate::new(human.clone()).with_timeout(Duration::from_secs(5));
    let ui = Arc::new(gate.with_lines(|_| {}));
    let mut mods = Mods::new(ModEnv::new(CWD, WorkspaceMode::Real, ui));
    mods.add(Arc::new(ApprovalMod::default()));
    let mods = Arc::new(mods);
    let rm = json!({"command": "rm -rf /p/doomed"});
    let p = ScriptedProvider::new([]);
    for id in ["a", "b", "c"] {
        p.push(tool_answer(&[(id, "Bash", rm.clone())]));
        p.push(text_answer("ok"));
    }
    let bash = Probe::new("Bash");
    let mut agent = agent(&p, &mods, vec![bash.clone()]);

    agent.run("delete it", &mut Vec::new()).await;
    assert!(bash.ran().is_empty());
    assert!(tool_results(&agent)[0].is_error);

    agent.run("again", &mut Vec::new()).await;
    assert_eq!(bash.ran().len(), 1);

    agent.run("and once more", &mut Vec::new()).await;
    assert_eq!(bash.ran().len(), 2);
    assert_eq!(human.prompts(), 2, "ALWAYS answered the third");
    assert_eq!(human.receipts(), 1);
}

#[tokio::test]
async fn plan_mode_refuses_writes_but_not_reads_and_the_plan_command_drives_it() {
    let states = Arc::new(Mutex::new(Vec::new()));
    let seen = states.clone();
    let ui = TestUi::new();
    let mut mods = Mods::new(env(ui.clone(), WorkspaceMode::Real));
    mods.add(Arc::new(PlanMod::new(Some(Arc::new(move |on| {
        seen.lock().unwrap().push(on)
    })))));
    let mods = Arc::new(mods);
    let read = ("r", "Read", json!({"file_path": "/p/plan.txt"}));
    let write = (
        "w",
        "Write",
        json!({"file_path": "/p/plan.txt", "content": "after"}),
    );
    let p = ScriptedProvider::new([
        tool_answer(&[read, write.clone()]),
        text_answer("planned"),
        tool_answer(&[write]),
        text_answer("done"),
    ]);
    let (reader, writer) = (Probe::read_only("Read"), Probe::new("Write"));
    let mut agent = agent(&p, &mods, vec![reader.clone(), writer.clone()]);

    // A host's plan toggle runs the same command as `/plan`.
    assert_eq!(mods.command("plan", "").await, CommandOutcome::Handled);
    assert_eq!(states.lock().unwrap().last(), Some(&true));
    agent.run("plan the change", &mut Vec::new()).await;
    assert!(p.requests()[0].system.contains(PLAN_PROMPT));
    let errors: Vec<bool> = tool_results(&agent).iter().map(|r| r.is_error).collect();
    assert_eq!(errors, [false, true]);
    assert!(writer.ran().is_empty());

    assert_eq!(mods.command("/plan", "off").await, CommandOutcome::Handled);
    assert_eq!(states.lock().unwrap().last(), Some(&false));
    agent.run("now do it", &mut Vec::new()).await;
    assert!(!p.requests()[2].system.contains(PLAN_PROMPT));
    assert_eq!(writer.ran().len(), 1);
    assert_eq!(ui.notices().len(), 2);
}

#[tokio::test]
async fn plan_with_a_topic_sends_the_topic_to_the_model() {
    let mut mods = Mods::new(env(TestUi::new(), WorkspaceMode::Sandbox));
    let plan = Arc::new(PlanMod::default());
    mods.add(plan.clone());
    match mods.command("plan", "  the migration ").await {
        CommandOutcome::Prompt(text) => assert!(text.contains("the migration")),
        other => panic!("nothing for the model: {other:?}"),
    }
    assert!(plan.is_on());
}

#[tokio::test]
async fn plan_mode_lets_the_hosts_planning_tools_through_and_no_others() {
    let mut mods = Mods::new(env(TestUi::new(), WorkspaceMode::Real));
    mods.add(Arc::new(PlanMod::default().with_planning_tools(["Notes"])));
    assert_eq!(mods.command("plan", "").await, CommandOutcome::Handled);
    let (notes, tasks) = (Probe::new("Notes"), Probe::new("TaskCreate"));
    let tools = wrapped(mods, vec![notes.clone(), tasks.clone()]);
    tools[0].execute(input(json!({})), &ctx()).await.unwrap();
    assert!(tools[1].execute(input(json!({})), &ctx()).await.is_err());
    assert_eq!((notes.ran().len(), tasks.ran().len()), (1, 0));
}
