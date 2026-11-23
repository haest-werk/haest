//! Mods in a conversation: the decorators around a real agent loop, the way a
//! host composes them.

use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

use haest_core::{
    Agent, AgentDeps, AgentOptions, Message, SharedDeps, ToolResultMessage, Tools,
    testing::{ScriptedProvider, text_answer, tool_answer},
    tool::{OnResume, Tool, ToolSource},
    workspace::{ShutWorkspace, WorkspaceMode},
};
use serde_json::json;

use crate::{
    ApprovalAnswer, ApprovalGate, ApprovalMod, CommandOutcome, ModEnv, ModRef, ModSystemPrompt,
    ModTools, Mods, PLAN_PROMPT, PlanMod, testing::*,
};

fn agent(p: &ScriptedProvider, mods: &Mods, tools: Tools) -> Agent {
    let deps = AgentDeps::new(
        SharedDeps::new(p.resolver(), CWD, ShutWorkspace),
        ModTools::new(tools, mods.clone()),
        ModSystemPrompt::new("You are haest.", mods.clone()),
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
    let ui = gate.mod_ui(|_| {});
    let env = ModEnv::new(CWD, WorkspaceMode::Real, ui);
    let mods = Mods::new(env, [ApprovalMod::new()]);
    let rm = json!({"command": "rm -rf /p/doomed"});
    let p = ScriptedProvider::new([]);
    for id in ["a", "b", "c"] {
        p.push(tool_answer(&[(id, "Bash", rm.clone())]));
        p.push(text_answer("ok"));
    }
    let bash = Probe::new("Bash");
    let mut agent = agent(&p, &mods, Tools::from_iter([bash.clone()]));

    let _ = agent.run("delete it", &mut Vec::new()).await;
    assert!(bash.ran().is_empty());
    assert!(tool_results(&agent)[0].is_error);

    let _ = agent.run("again", &mut Vec::new()).await;
    assert_eq!(bash.ran().len(), 1);

    let _ = agent.run("and once more", &mut Vec::new()).await;
    assert_eq!(bash.ran().len(), 2);
    assert_eq!(human.prompts(), 2, "ALWAYS answered the third");
    assert_eq!(human.receipts(), 1);
}

#[tokio::test]
async fn plan_mode_refuses_writes_but_not_reads_and_the_plan_command_drives_it() {
    let states = Arc::new(Mutex::new(Vec::new()));
    let seen = states.clone();
    let ui = TestUi::new();
    let plan = PlanMod::new().with_listener(move |on| seen.lock().unwrap().push(on));
    let mods = Mods::new(env(ui.clone(), WorkspaceMode::Real), [plan]);
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
    let mut agent = agent(
        &p,
        &mods,
        Tools::from_iter([reader.clone(), writer.clone()]),
    );

    // A host's plan toggle runs the same command as `/plan`.
    assert_eq!(mods.command("plan", "").await, CommandOutcome::Handled);
    assert_eq!(states.lock().unwrap().last(), Some(&true));
    let _ = agent.run("plan the change", &mut Vec::new()).await;
    assert!(p.requests()[0].system.contains(PLAN_PROMPT));
    let errors: Vec<bool> = tool_results(&agent).iter().map(|r| r.is_error).collect();
    assert_eq!(errors, [false, true]);
    assert!(writer.ran().is_empty());

    assert_eq!(mods.command("/plan", "off").await, CommandOutcome::Handled);
    assert_eq!(states.lock().unwrap().last(), Some(&false));
    let _ = agent.run("now do it", &mut Vec::new()).await;
    assert!(!p.requests()[2].system.contains(PLAN_PROMPT));
    assert_eq!(writer.ran().len(), 1);
    assert_eq!(ui.infos().len(), 2);
}

#[tokio::test]
async fn plan_with_a_topic_sends_the_topic_to_the_model() {
    // The host keeps an Arc of the mod, to look at it.
    let plan = Arc::new(PlanMod::new());
    let mods = Mods::new(env(TestUi::new(), WorkspaceMode::Sandbox), [plan.clone()]);
    match mods.command("plan", "  the migration ").await {
        CommandOutcome::Prompt(text) => assert!(text.contains("the migration")),
        other => panic!("nothing for the model: {other:?}"),
    }
    assert!(plan.is_on());
}

#[tokio::test]
async fn plan_mode_lets_the_hosts_planning_tools_through_and_no_others() {
    let plan = PlanMod::new().with_planning_tools(["Notes"]);
    let mods = Mods::new(env(TestUi::new(), WorkspaceMode::Real), [plan]);
    assert_eq!(mods.command("plan", "").await, CommandOutcome::Handled);
    let (notes, tasks) = (Probe::new("Notes"), Probe::new("TaskCreate"));
    let tools = wrapped(mods, Tools::from_iter([notes.clone(), tasks.clone()]));
    tools[0].execute(input(json!({})), &ctx()).await.unwrap();
    assert!(tools[1].execute(input(json!({})), &ctx()).await.is_err());
    assert_eq!((notes.ran().len(), tasks.ran().len()), (1, 0));
}

/// Advertises its first tool only and keeps the rest back, callable by name
/// (as a deferral does).
struct KeptBack(Tools);

impl ToolSource for KeptBack {
    fn tools(&self) -> Tools {
        self.0[..1].iter().cloned().collect()
    }
    fn callable(&self) -> Tools {
        self.0.clone()
    }
}

/// Regression: ModTools offered only the advertised tools as callable, so
/// over a source that keeps tools back (a `DeferredTools`) a tool the model
/// called by name was unknown.
#[tokio::test]
async fn a_tool_the_source_keeps_back_runs_by_name_inside_the_mods_hooks() {
    let mods = Mods::new(env(TestUi::new(), WorkspaceMode::Real), [PlanMod::new()]);
    let write = || tool_answer(&[("w", "Write", json!({"file_path": "/p/a.txt"}))]);
    let p = ScriptedProvider::new([write(), text_answer("ok"), write(), text_answer("ok")]);
    let (reader, writer) = (Probe::read_only("Read"), Probe::new("Write"));
    let kept = KeptBack(Tools::from_iter([reader, writer.clone()]));
    let deps = AgentDeps::new(
        SharedDeps::new(p.resolver(), CWD, ShutWorkspace),
        ModTools::new(kept, mods.clone()),
        "You are haest.",
    );
    let mut agent = Agent::new(deps, AgentOptions::new("scripted:m"));

    let _ = agent.run("write it", &mut Vec::new()).await;
    assert_eq!(writer.ran().len(), 1, "{:?}", tool_results(&agent));

    // Plan mode refuses a write: the kept-back tool runs inside the hooks.
    assert_eq!(mods.command("plan", "").await, CommandOutcome::Handled);
    let _ = agent.run("write it again", &mut Vec::new()).await;
    assert_eq!(writer.ran().len(), 1);
    assert!(tool_results(&agent)[1].is_error);
}

/// A payment tool that can look up what it did when a durable run resumes it.
struct Pay;

#[async_trait::async_trait]
impl Tool for Pay {
    fn name(&self) -> &str {
        "Pay"
    }
    fn description(&self) -> String {
        "pay".into()
    }
    fn parameters(&self) -> serde_json::Value {
        json!({"type": "object"})
    }
    fn kind(&self) -> &str {
        "agent.unknown"
    }
    fn on_resume(&self) -> OnResume {
        OnResume::Lookup
    }
    async fn execute(
        &self,
        _: haest_core::tool::JsonMap,
        _: &haest_core::ToolContext,
    ) -> Result<haest_core::ToolOutput, haest_core::ToolError> {
        Ok(haest_core::ToolOutput::text("paid"))
    }
}

#[test]
fn a_wrapped_tool_keeps_what_it_does_on_resume() {
    let mods = Mods::new(
        env(TestUi::new(), WorkspaceMode::Real),
        Vec::<ModRef>::new(),
    );
    let tools = wrapped(
        mods,
        Tools::new()
            .with(Pay)
            .with(Probe::new("Write"))
            .with(Probe::read_only("Read")),
    );
    assert_eq!(find(&tools, "Pay").on_resume(), OnResume::Lookup);
    assert_eq!(find(&tools, "Write").on_resume(), OnResume::Report);
    assert_eq!(find(&tools, "Read").on_resume(), OnResume::Rerun);
}
