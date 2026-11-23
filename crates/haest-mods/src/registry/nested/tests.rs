//! A [`Mods`] inside another runs its mods in its place: every decision
//! takes the effect it takes flat, in the same order, a failure is reported
//! once under the failing mod's own name, and a stop names that mod too.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use haest_core::{
    Agent, NoHooks, StopReason, Tools,
    error::BoxError,
    testing::{ScriptedProvider, text_answer},
    tool::{ToolContext, ToolError, ToolOutput},
    workspace::WorkspaceMode,
};
use serde_json::json;
use tokio_util::sync::CancellationToken;

use crate::{
    api::{Before, Mod, ModRef, ModToolCall, Step},
    env::ModEnv,
    plan::PlanMod,
    registry::{CommandOutcome, Mods},
    testkit::*,
};

/// `mods`, nested one level down in a `Mods` of their own.
fn nested(env: &ModEnv, mods: impl IntoIterator<Item = ModRef>) -> Mods {
    Mods::new(env.clone(), [Mods::new(env.clone(), mods)])
}

#[tokio::test]
async fn a_nested_mods_takes_every_decision_as_its_mods_take_it_flat() {
    for DecisionCase {
        event,
        answer,
        took_effect,
    } in decision_cases()
    {
        let haest = env(TestUi::new(), WorkspaceMode::Sandbox);
        let hooks = || ModRef::new(hooks_answering(event, answer.clone()));
        let flat = effect(event, Mods::new(haest.clone(), [hooks()]), "hooks").await;
        let inside = effect(event, nested(&haest, [hooks()]), "hooks").await;
        assert_eq!(inside, flat, "{event} answering {answer}");
        assert!(took_effect(&inside), "{event} answering {answer}: {inside}");
    }
}

type Log = Arc<Mutex<Vec<String>>>;

/// Notes its name in every event it hears, and adds it to the human's text
/// and the system prompt.
struct Note(&'static str, Log);

impl Note {
    fn noted(&self) {
        self.1.lock().unwrap().push(self.0.to_string());
    }
}

#[async_trait]
impl Mod for Note {
    fn name(&self) -> &str {
        self.0
    }
    async fn tool_before(
        &self,
        _call: &ModToolCall,
        _ctx: &ToolContext,
        _haest: &ModEnv,
    ) -> Result<Before, BoxError> {
        self.noted();
        Ok(Before::Pass)
    }
    async fn prompt_submit(&self, text: &str, _haest: &ModEnv) -> Result<Option<String>, BoxError> {
        Ok(Some(format!("{text} {}", self.0)))
    }
    async fn system_prompt(&self, _haest: &ModEnv) -> Result<Option<String>, BoxError> {
        Ok(Some(self.0.into()))
    }
    async fn model_before(
        &self,
        _agent: &mut Agent,
        _cancel: &CancellationToken,
        _haest: &ModEnv,
    ) -> Result<Step, BoxError> {
        self.noted();
        Ok(Step::Pass)
    }
}

#[tokio::test]
async fn a_nested_mods_runs_its_mods_in_its_place_among_the_others() {
    let log = Log::default();
    let note = |name| ModRef::new(Note(name, log.clone()));
    let haest = env(TestUi::new(), WorkspaceMode::Sandbox);
    let middle = Mods::new(haest.clone(), [note("b"), note("c")]);
    let mods = Mods::new(haest, [note("a"), ModRef::new(middle), note("d")]);

    assert_eq!(mods.rewrite_prompt("hi").await, "hi a b c d");
    assert_eq!(mods.system_append().await, "a\n\nb\n\nc\n\nd");
    let tools = wrapped(mods.clone(), Tools::from_iter([Probe::new("Echo")]));
    tools[0].execute(input(json!({})), &ctx()).await.unwrap();
    let p = ScriptedProvider::new([text_answer("done")]);
    let _ = looped(&p, NoHooks, &mods, Tools::new())
        .run("go", &mut Vec::new())
        .await;
    let heard = log.lock().unwrap().clone();
    assert_eq!(heard, ["a", "b", "c", "d", "a", "b", "c", "d"]);
}

/// Fails every event it hooks.
struct Fails;

#[async_trait]
impl Mod for Fails {
    fn name(&self) -> &str {
        "fails"
    }
    async fn tool_before(
        &self,
        _call: &ModToolCall,
        _ctx: &ToolContext,
        _haest: &ModEnv,
    ) -> Result<Before, BoxError> {
        Err("the guard broke".into())
    }
    async fn tool_after(
        &self,
        _call: &ModToolCall,
        _output: &ToolOutput,
        _ctx: &ToolContext,
        _haest: &ModEnv,
    ) -> Result<Option<ToolOutput>, BoxError> {
        Err("the rewrite broke".into())
    }
    async fn model_before(
        &self,
        _agent: &mut Agent,
        _cancel: &CancellationToken,
        _haest: &ModEnv,
    ) -> Result<Step, BoxError> {
        Err("the step check broke".into())
    }
    fn commands(&self) -> Vec<String> {
        vec!["broken".into()]
    }
    async fn command(&self, _: &str, _: &str, _: &ModEnv) -> Result<Option<String>, BoxError> {
        Err("the command broke".into())
    }
}

#[tokio::test]
async fn a_nested_mods_failure_is_reported_once_under_the_failing_mods_name() {
    let ui = TestUi::new();
    let haest = env(ui.clone(), WorkspaceMode::Sandbox);
    let mods = nested(&haest, [ModRef::new(Fails)]);

    let echo = Probe::new("Echo");
    let tools = wrapped(mods.clone(), Tools::from_iter([echo.clone()]));
    match tools[0].execute(input(json!({})), &ctx()).await {
        Err(ToolError::Failed(why)) => {
            assert!(why.contains("the mod fails failed"), "{why}");
            assert!(
                why.contains("the guard broke") && !why.contains("mods"),
                "{why}"
            );
        }
        other => panic!("not refused as failed: {other:?}"),
    }
    assert!(echo.ran().is_empty());
    let p = ScriptedProvider::new([text_answer("done")]);
    let end = looped(&p, NoHooks, &mods, Tools::new())
        .run("go", &mut Vec::new())
        .await;
    assert_eq!((end.stop_reason, end.error), (StopReason::EndTurn, None));
    assert_eq!(mods.command("broken", "").await, CommandOutcome::Handled);

    let errors = ui.errors();
    assert_eq!(errors.len(), 3, "{errors:?}");
    assert!(
        errors.iter().all(|e| e.contains("Mod fails failed")),
        "{errors:?}"
    );
}

/// Adds a tool and a command, and hides the host's Bash.
struct Adds;

#[async_trait]
impl Mod for Adds {
    fn name(&self) -> &str {
        "adds"
    }
    fn tools(&self) -> Tools {
        Tools::from_iter([Probe::new("Shout")])
    }
    fn disabled_tools(&self) -> Vec<String> {
        vec!["Bash".into()]
    }
}

#[tokio::test]
async fn a_nested_mods_tools_hidden_tools_and_commands_are_the_outer_ones() {
    let haest = env(TestUi::new(), WorkspaceMode::Sandbox);
    let mods = nested(&haest, [ModRef::new(Adds), ModRef::new(PlanMod::new())]);
    let tools = wrapped(mods.clone(), Tools::from_iter([Probe::new("Bash")]));
    assert_eq!(names(&tools), ["Shout"]);
    assert_eq!(mods.disabled_tools(), ["Bash"]);
    assert_eq!(mods.commands(), ["plan"]);
    let planned = mods.command("/plan", "the move").await;
    assert!(matches!(planned, CommandOutcome::Prompt(t) if t.contains("the move")));
}
