use async_trait::async_trait;
use haest_core::{
    ToolRef, Tools,
    deps::SystemPrompt,
    error::BoxError,
    tool::{ToolContext, ToolError, ToolOutput},
    workspace::WorkspaceMode,
};
use serde_json::{Value, json};

use super::*;
use crate::{
    api::{Before, Mod, ModRef, ModToolCall},
    decorate::ModSystemPrompt,
    env::ModEnv,
    plan::PlanMod,
    testkit::*,
};

/// The TS test's mod: refuse `bad`, add `!` to anything else, shout the output.
struct Rewriter;

#[async_trait]
impl Mod for Rewriter {
    fn name(&self) -> &str {
        "rewriter"
    }
    async fn tool_before(
        &self,
        call: &ModToolCall,
        _ctx: &ToolContext,
        _haest: &ModEnv,
    ) -> Result<Before, BoxError> {
        let v = call.input.get("v").and_then(Value::as_str).unwrap_or("");
        Ok(if v == "bad" {
            Before::Deny("no bad".into())
        } else {
            Before::Input(input(json!({"v": format!("{v}!")})))
        })
    }
    async fn tool_after(
        &self,
        _call: &ModToolCall,
        output: &ToolOutput,
        _ctx: &ToolContext,
        _haest: &ModEnv,
    ) -> Result<Option<ToolOutput>, BoxError> {
        Ok(Some(ToolOutput {
            content: output.content.to_uppercase(),
            ..output.clone()
        }))
    }
}

#[tokio::test]
async fn tool_before_can_refuse_or_rewrite_a_call_and_tool_after_can_rewrite_its_output() {
    let mods = Mods::new(env(TestUi::new(), WorkspaceMode::Real), [Rewriter]);
    let echo = Probe::new("Echo");
    let tools = wrapped(mods, Tools::from_iter([echo.clone()]));
    let ok = tools[0].execute(input(json!({"v": "hi"})), &ctx()).await;
    assert_eq!(ok.unwrap().content, "ECHO:HI!");
    match tools[0].execute(input(json!({"v": "bad"})), &ctx()).await {
        Err(ToolError::Input(m)) => assert!(m.contains("no bad")),
        other => panic!("not refused: {other:?}"),
    }
    assert_eq!(echo.ran().len(), 1, "the refused call never ran");
}

/// The TS test's second mod: a prompt rule, a house rule, a tool and a command.
struct HouseRules(ToolRef);

#[async_trait]
impl Mod for HouseRules {
    fn name(&self) -> &str {
        "house"
    }
    async fn prompt_submit(&self, text: &str, _k: &ModEnv) -> Result<Option<String>, BoxError> {
        Ok(Some(format!("{text} (be brief)")))
    }
    async fn system_prompt(&self, _k: &ModEnv) -> Result<Option<String>, BoxError> {
        Ok(Some("House rule: British spelling.".into()))
    }
    fn tools(&self) -> Tools {
        Tools::new().with(self.0.clone())
    }
    fn commands(&self) -> Vec<String> {
        vec!["/hello".into()]
    }
    async fn command(&self, _n: &str, args: &str, _k: &ModEnv) -> Result<Option<String>, BoxError> {
        Ok(Some(format!("say hello to {args}")))
    }
}

#[tokio::test]
async fn prompt_system_prompt_tools_and_commands_from_a_mod_all_take_effect() {
    let house = HouseRules(Probe::new("Shout").into());
    let mods = Mods::new(env(TestUi::new(), WorkspaceMode::Sandbox), [house]);
    assert_eq!(mods.rewrite_prompt("fix it").await, "fix it (be brief)");
    assert_eq!(mods.system_append().await, "House rule: British spelling.");
    assert_eq!(mods.commands(), ["hello"]);
    assert_eq!(
        mods.command("hello", "bob").await,
        CommandOutcome::Prompt("say hello to bob".into())
    );
    assert_eq!(mods.command("/nope", "").await, CommandOutcome::Unknown);
    let prompt = ModSystemPrompt::new("base", mods.clone())
        .system_prompt()
        .await;
    assert!(prompt.starts_with("base") && prompt.ends_with("House rule: British spelling."));
    let names: Vec<String> = crate::ModTools::wrap(&mods, Tools::from_iter([Probe::new("Echo")]))
        .iter()
        .map(|t| t.name().to_string())
        .collect();
    assert_eq!(names, ["Echo", "Shout"]);
}

/// Fails in every event.
struct Broken;

#[async_trait]
impl Mod for Broken {
    fn name(&self) -> &str {
        "broken"
    }
    async fn tool_before(
        &self,
        call: &ModToolCall,
        _c: &ToolContext,
        _k: &ModEnv,
    ) -> Result<Before, BoxError> {
        if call.input.contains_key("guarded") {
            return Err("boom".into());
        }
        Ok(Before::Pass)
    }
    async fn tool_after(
        &self,
        _c: &ModToolCall,
        _o: &ToolOutput,
        _x: &ToolContext,
        _k: &ModEnv,
    ) -> Result<Option<ToolOutput>, BoxError> {
        Err("boom".into())
    }
    async fn prompt_submit(&self, _t: &str, _k: &ModEnv) -> Result<Option<String>, BoxError> {
        Err("boom".into())
    }
}

#[tokio::test]
async fn a_failing_mod_is_reported_and_never_lets_a_guarded_call_through() {
    let ui = TestUi::new();
    let mods = Mods::new(env(ui.clone(), WorkspaceMode::Real), [Broken]);
    let echo = Probe::new("Echo");
    let tools = wrapped(mods, Tools::from_iter([echo.clone()]));
    let refused = tools[0]
        .execute(input(json!({"guarded": true})), &ctx())
        .await;
    assert!(refused.is_err());
    assert!(echo.ran().is_empty());
    // tool.after failing after the tool ran: the output stands, the call is not a failure.
    let ran = tools[0].execute(input(json!({"v": "x"})), &ctx()).await;
    assert_eq!(ran.unwrap().content, "echo:x");
    assert_eq!(echo.ran().len(), 1);
    assert_eq!(ui.errors().len(), 2);
    assert!(ui.errors().iter().all(|e| e.contains("broken")));
}

#[tokio::test]
async fn a_failing_prompt_hook_leaves_the_text_as_it_was() {
    let ui = TestUi::new();
    let mods = Mods::new(
        env(ui.clone(), WorkspaceMode::Real),
        [
            ModRef::new(Broken),
            ModRef::new(HouseRules(Probe::new("Shout").into())),
        ],
    );
    assert_eq!(mods.rewrite_prompt("fix it").await, "fix it (be brief)");
    assert_eq!(ui.errors().len(), 1);
}

/// Fails `system.prompt` with an error whose cause says what broke.
struct Unread;

#[derive(Debug, thiserror::Error)]
#[error("the house rules cannot be read")]
struct RulesUnread(#[source] std::io::Error);

#[async_trait]
impl Mod for Unread {
    fn name(&self) -> &str {
        "unread"
    }
    async fn system_prompt(&self, _k: &ModEnv) -> Result<Option<String>, BoxError> {
        Err(RulesUnread(std::io::Error::other("disk gone")).into())
    }
}

#[tokio::test]
async fn a_mod_failure_reaches_the_human_with_its_cause() {
    let ui = TestUi::new();
    let mods = Mods::new(env(ui.clone(), WorkspaceMode::Real), [Unread]);
    assert_eq!(mods.system_append().await, "");
    let errors = ui.errors();
    assert_eq!(errors.len(), 1);
    assert!(errors[0].contains("disk gone"), "{errors:?}");
}

#[tokio::test]
async fn an_interrupt_while_a_mod_decides_is_not_reported_as_a_mod_failure() {
    let ui = TestUi::new();
    let mods = Mods::new(env(ui.clone(), WorkspaceMode::Real), [Broken]);
    let tools = wrapped(mods, Tools::from_iter([Probe::new("Echo")]));
    let c = ctx();
    c.cancel().cancel();
    let out = tools[0].execute(input(json!({"guarded": true})), &c).await;
    assert!(matches!(out, Err(ToolError::Input(_))));
    assert!(ui.errors().is_empty());
}

#[tokio::test]
async fn mods_is_a_handle_its_clones_run_the_same_mods() {
    let shared = Mods::new(
        env(TestUi::new(), WorkspaceMode::Sandbox),
        [ModRef::new(PlanMod::new()), ModRef::new(Unread)],
    );
    let host = shared.clone();
    // Plan mode switched through one clone is on for the other: the same mod.
    assert_eq!(host.command("plan", "").await, CommandOutcome::Handled);
    assert!(!shared.system_append().await.is_empty());
    assert_eq!(host.names(), ["plan", "unread"]);
    assert!(format!("{shared:?}").contains("ModRef(\"plan\")"));
}
