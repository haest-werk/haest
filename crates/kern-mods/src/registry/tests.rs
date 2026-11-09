use std::sync::Arc;

use async_trait::async_trait;
use kern_core::{
    deps::SystemPrompt,
    tool::{Tool, ToolContext, ToolError, ToolOutput},
    workspace::WorkspaceMode,
};
use serde_json::{Value, json};

use super::*;
use crate::{
    api::{Before, Mod, ModEnv, ModError, ModToolCall},
    decorate::ModSystemPrompt,
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
        _kern: &ModEnv,
    ) -> Result<Before, ModError> {
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
        _kern: &ModEnv,
    ) -> Result<Option<ToolOutput>, ModError> {
        Ok(Some(ToolOutput {
            content: output.content.to_uppercase(),
            ..output.clone()
        }))
    }
}

#[tokio::test]
async fn tool_before_can_refuse_or_rewrite_a_call_and_tool_after_can_rewrite_its_output() {
    let mut mods = Mods::new(env(TestHost::new(), WorkspaceMode::Real));
    mods.add(Arc::new(Rewriter));
    let echo = Probe::new("Echo");
    let tools = wrapped(mods, vec![echo.clone()]);
    let ok = tools[0].execute(input(json!({"v": "hi"})), &ctx()).await;
    assert_eq!(ok.unwrap().content, "ECHO:HI!");
    match tools[0].execute(input(json!({"v": "bad"})), &ctx()).await {
        Err(ToolError::Input(m)) => assert!(m.contains("no bad")),
        other => panic!("not refused: {other:?}"),
    }
    assert_eq!(echo.ran().len(), 1, "the refused call never ran");
}

/// The TS test's second mod: a prompt rule, a house rule, a tool and a command.
struct HouseRules(Arc<dyn Tool>);

#[async_trait]
impl Mod for HouseRules {
    fn name(&self) -> &str {
        "house"
    }
    async fn prompt_submit(&self, text: &str, _k: &ModEnv) -> Result<Option<String>, ModError> {
        Ok(Some(format!("{text} (be brief)")))
    }
    async fn system_prompt(&self, _k: &ModEnv) -> Result<Option<String>, ModError> {
        Ok(Some("House rule: British spelling.".into()))
    }
    fn tools(&self) -> Vec<Arc<dyn Tool>> {
        vec![self.0.clone()]
    }
    fn commands(&self) -> Vec<String> {
        vec!["/hello".into()]
    }
    async fn command(&self, _n: &str, args: &str, _k: &ModEnv) -> Result<Option<String>, ModError> {
        Ok(Some(format!("say hello to {args}")))
    }
}

#[tokio::test]
async fn prompt_system_prompt_tools_and_commands_from_a_mod_all_take_effect() {
    let mut mods = Mods::new(env(TestHost::new(), WorkspaceMode::Sandbox));
    mods.add(Arc::new(HouseRules(Probe::new("Shout"))));
    assert_eq!(mods.rewrite_prompt("fix it").await, "fix it (be brief)");
    assert_eq!(mods.system_append().await, "House rule: British spelling.");
    assert_eq!(mods.commands(), ["hello"]);
    assert_eq!(
        mods.command("hello", "bob").await,
        CommandOutcome::Prompt("say hello to bob".into())
    );
    assert_eq!(mods.command("/nope", "").await, CommandOutcome::Unknown);
    let mods = Arc::new(mods);
    let base: Arc<dyn SystemPrompt> = Arc::new("base".to_string());
    let prompt = ModSystemPrompt::new(base, mods.clone())
        .system_prompt()
        .await;
    assert!(prompt.starts_with("base") && prompt.ends_with("House rule: British spelling."));
    let names: Vec<String> = crate::ModTools::wrap(&mods, vec![Probe::new("Echo")])
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
    ) -> Result<Before, ModError> {
        if call.input.contains_key("guarded") {
            return Err(ModError::new("boom"));
        }
        Ok(Before::Pass)
    }
    async fn tool_after(
        &self,
        _c: &ModToolCall,
        _o: &ToolOutput,
        _x: &ToolContext,
        _k: &ModEnv,
    ) -> Result<Option<ToolOutput>, ModError> {
        Err(ModError::new("boom"))
    }
    async fn prompt_submit(&self, _t: &str, _k: &ModEnv) -> Result<Option<String>, ModError> {
        Err(ModError::new("boom"))
    }
}

#[tokio::test]
async fn a_failing_mod_is_reported_and_never_lets_a_guarded_call_through() {
    let host = TestHost::new();
    let mut mods = Mods::new(env(host.clone(), WorkspaceMode::Real));
    mods.add(Arc::new(Broken));
    let echo = Probe::new("Echo");
    let tools = wrapped(mods, vec![echo.clone()]);
    let refused = tools[0]
        .execute(input(json!({"guarded": true})), &ctx())
        .await;
    assert!(refused.is_err());
    assert!(echo.ran().is_empty());
    // tool.after failing after the tool ran: the output stands, the call is not a failure.
    let ran = tools[0].execute(input(json!({"v": "x"})), &ctx()).await;
    assert_eq!(ran.unwrap().content, "echo:x");
    assert_eq!(echo.ran().len(), 1);
    assert_eq!(host.errors().len(), 2);
    assert!(host.errors().iter().all(|e| e.contains("broken")));
}

#[tokio::test]
async fn a_failing_prompt_hook_leaves_the_text_as_it_was() {
    let host = TestHost::new();
    let mut mods = Mods::new(env(host.clone(), WorkspaceMode::Real));
    mods.add(Arc::new(Broken));
    mods.add(Arc::new(HouseRules(Probe::new("Shout"))));
    assert_eq!(mods.rewrite_prompt("fix it").await, "fix it (be brief)");
    assert_eq!(host.errors().len(), 1);
}

#[tokio::test]
async fn an_interrupt_while_a_mod_decides_is_not_reported_as_a_mod_failure() {
    let host = TestHost::new();
    let mut mods = Mods::new(env(host.clone(), WorkspaceMode::Real));
    mods.add(Arc::new(Broken));
    let tools = wrapped(mods, vec![Probe::new("Echo")]);
    let c = ctx();
    c.cancel.cancel();
    let out = tools[0].execute(input(json!({"guarded": true})), &c).await;
    assert!(matches!(out, Err(ToolError::Input(_))));
    assert!(host.errors().is_empty());
}
