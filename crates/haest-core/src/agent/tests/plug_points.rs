//! What a host plugs into the loop: a hook before each model call that ends
//! the turn or replaces the history, and a tool source that keeps tools back.

use super::*;

struct Compacting;
impl ToolServices for Compacting {}
#[async_trait]
impl AgentHooks for Compacting {
    async fn before_model_call(
        &self,
        agent: &mut Agent,
        _: &CancellationToken,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let last = agent.history().last().cloned();
        agent.replace_history(last.into_iter().collect());
        Ok(())
    }
}

/// A host that cannot go on (its summary failed): the turn ends there.
struct Refusing;
impl ToolServices for Refusing {}
#[async_trait]
impl AgentHooks for Refusing {
    async fn before_model_call(
        &self,
        _: &mut Agent,
        _: &CancellationToken,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        Err("couldn't summarise the earlier conversation".into())
    }
}

#[tokio::test]
async fn the_hook_before_a_model_call_may_end_the_turn_as_an_error_without_the_call() {
    let p = ScriptedProvider::new([text_answer("never")]);
    let (mut agent, _) = agent_with(&p, Opts::default());
    agent.deps_mut().shared.hooks = Arc::new(Refusing);
    let events = collect(&mut agent, "go").await;
    let (reason, _, error) = end(&events);
    assert_eq!(reason, StopReason::Error);
    assert_eq!(
        error.as_deref(),
        Some("couldn't summarise the earlier conversation")
    );
    assert!(p.requests().is_empty(), "no model call was made");
}

#[tokio::test]
async fn the_hook_before_each_model_call_may_replace_history() {
    let p = ScriptedProvider::new([text_answer("a")]);
    let (mut agent, _) = agent_with(&p, Opts::default());
    agent.replace_history(vec![Message::user("old"), Message::user("older")]);
    agent.deps_mut().shared.hooks = Arc::new(Compacting);
    collect(&mut agent, "new").await;
    let sent = &p.requests()[0].messages;
    assert_eq!(sent.len(), 1);
    assert!(matches!(&sent[0], Message::User(u) if u.content == "new"));
}

/// Advertises its first tool only; the rest it keeps back, callable by name.
struct KeptBack(Vec<Arc<dyn crate::tool::Tool>>);

impl crate::tool::ToolSource for KeptBack {
    fn tools(&self) -> Vec<Arc<dyn crate::tool::Tool>> {
        self.0[..1].to_vec()
    }
    fn callable(&self) -> Vec<Arc<dyn crate::tool::Tool>> {
        self.0.clone()
    }
}

#[tokio::test]
async fn a_tool_the_source_keeps_back_runs_when_the_model_calls_it_by_name() {
    let p = ScriptedProvider::new([
        tool_answer(&[("c1", "Hidden", json!({}))]),
        text_answer("done"),
    ]);
    let (mut agent, _) = agent_with(&p, Opts::default());
    let hidden = FnTool::arc("Hidden", true, |_, _| async {
        Ok(crate::tool::ToolOutput::text("ran"))
    });
    agent.deps.tools = Arc::new(KeptBack(vec![echo_tool(), hidden]));
    collect(&mut agent, "go").await;
    let sent: Vec<String> = p.requests()[0]
        .tools
        .iter()
        .map(|t| t.name.clone())
        .collect();
    assert_eq!(sent, ["Echo"], "only the advertised tool is sent");
    assert!(
        agent
            .history()
            .iter()
            .any(|m| matches!(m, Message::Tool(r) if r.content == "ran" && !r.is_error)),
        "the kept-back tool ran: {:?}",
        agent.history()
    );
}
