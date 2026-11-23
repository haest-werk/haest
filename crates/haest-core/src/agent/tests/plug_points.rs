//! What a host plugs into the loop: a hook before each model call that ends
//! the turn or replaces the history, and a tool source that keeps tools back.

use super::*;
use crate::error::BoxError;

struct Compacting;
#[async_trait]
impl AgentHooks for Compacting {
    async fn before_model_call(
        &self,
        agent: &mut Agent,
        _: &CancellationToken,
    ) -> Result<(), BoxError> {
        let last = agent.history().last().cloned();
        agent.replace_history(last.into_iter().collect());
        Ok(())
    }
}

/// A host that cannot go on (its summary failed): the turn ends there.
struct Refusing;
#[async_trait]
impl AgentHooks for Refusing {
    async fn before_model_call(
        &self,
        _: &mut Agent,
        _: &CancellationToken,
    ) -> Result<(), BoxError> {
        Err("couldn't summarise the earlier conversation".into())
    }
}

#[tokio::test]
async fn the_hook_before_a_model_call_may_end_the_turn_as_an_error_without_the_call() {
    let p = ScriptedProvider::new([text_answer("never")]);
    let (mut agent, _) = agent_with(&p, Opts::default());
    agent.map_deps(|d| d.with_hooks(Refusing));
    let events = collect(&mut agent, "go").await;
    let (reason, _, error) = end(&events);
    assert_eq!(reason, StopReason::Error);
    assert_eq!(
        error.as_deref(),
        Some("couldn't summarise the earlier conversation")
    );
    assert!(p.requests().is_empty(), "no model call was made");
}

/// What a hook's error says, over the cause that says what broke.
#[derive(Debug, thiserror::Error)]
#[error("cannot go on")]
struct CannotGoOn(#[source] std::io::Error);

/// A host whose error has a cause.
struct FailsWithACause;
#[async_trait]
impl AgentHooks for FailsWithACause {
    async fn before_model_call(
        &self,
        _: &mut Agent,
        _: &CancellationToken,
    ) -> Result<(), BoxError> {
        Err(CannotGoOn(std::io::Error::other("the disk is full")).into())
    }
}

/// Regression: the turn's error was the hook error's own text alone, so the
/// cause that said what broke never reached the human.
#[tokio::test]
async fn a_hook_errors_cause_reaches_the_turns_error() {
    let p = ScriptedProvider::new([text_answer("never")]);
    let (mut agent, _) = agent_with(&p, Opts::default());
    agent.map_deps(|d| d.with_hooks(FailsWithACause));
    let ended = agent.run("go", &mut Vec::new()).await;
    assert_eq!(ended.stop_reason, StopReason::Error);
    let error = ended.error.unwrap_or_default();
    assert!(error.contains("the disk is full"), "{error}");
}

#[tokio::test]
async fn the_hook_before_each_model_call_may_replace_history() {
    let p = ScriptedProvider::new([text_answer("a")]);
    let (mut agent, _) = agent_with(&p, Opts::default());
    agent.replace_history(vec![Message::user("old"), Message::user("older")]);
    agent.map_deps(|d| d.with_hooks(Compacting));
    collect(&mut agent, "new").await;
    let sent = &p.requests()[0].messages;
    assert_eq!(sent.len(), 1);
    assert!(matches!(&sent[0], Message::User(u) if u.content == "new"));
}

/// Advertises its first tool only; the rest it keeps back, callable by name.
struct KeptBack(crate::tool::Tools);

impl crate::tool::ToolSource for KeptBack {
    fn tools(&self) -> crate::tool::Tools {
        self.0[..1].iter().cloned().collect()
    }
    fn callable(&self) -> crate::tool::Tools {
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
    agent.map_deps(|d| d.with_tools(KeptBack(vec![echo_tool(), hidden].into())));
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

/// A catalog that knows every model's window as `self.0`.
struct Window(u64);
impl crate::catalog::ModelFacts for Window {
    fn model_info(&self, spec: &str) -> Option<crate::catalog::ModelInfo> {
        Some(crate::catalog::ModelInfo::new(spec, self.0))
    }
}

/// One window for every reader (a hook's compaction, the idle tick, the
/// loop events' view): the catalog's, else the host's turn caps'.
#[test]
fn the_context_window_is_the_catalogs_else_the_turn_caps() {
    let p = ScriptedProvider::new([]);
    let (mut agent, _) = agent_with(&p, Opts::default());
    assert_eq!(agent.context_window(), None, "neither knows it");
    let caps = crate::caps::TurnCaps {
        context_window: 100_000,
        ..Default::default()
    };
    agent.map_deps(|d| d.with_turn_caps(caps));
    assert_eq!(agent.context_window(), Some(100_000), "the turn caps alone");
    agent.map_deps(|d| d.with_facts(Window(200_000)));
    assert_eq!(agent.context_window(), Some(200_000), "the catalog first");
    agent.map_deps(|d| d.with_facts(Window(0)));
    assert_eq!(
        agent.context_window(),
        Some(100_000),
        "a window of 0 is unknown"
    );
}
