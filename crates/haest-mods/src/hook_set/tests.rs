//! [`HookSet`](super::HookSet): what a failed or late hook does under each
//! `on_error` (on_error.rs); order, first wins, matchers and interrupts
//! (order.rs); the parity guard (parity.rs); the session every hook is
//! handed (session.rs); what `turn.end` hands a hook (turn_end.rs). That
//! every decision takes the
//! same effect from a `.ts` mod and from a `HookSet` closure is haest-bridges'
//! test, over the testing module's decision cases.

use std::sync::Arc;

use haest_core::{
    Agent, AgentDeps, AgentOptions, Message, NoHooks, SharedDeps, StopReason, ToolOutput, Tools,
    TurnOutcome,
    error::BoxError,
    testing::{ScriptedProvider, text_answer},
    tool::JsonMap,
    workspace::{ShutWorkspace, WorkspaceMode},
};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use crate::{
    api::{Mod, ModToolCall},
    env::ModEnv,
    events::{Event, ModEvent},
    hook::Hook,
    hook_set::HookSet,
    registry::Mods,
    testing::*,
};

mod on_error;
mod order;
mod parity;
mod session;
mod turn_end;

/// `event` fired on `m` itself, through its `Mod` method and outside any
/// [`Mods`], as a host may fire it: on an `Echo` call, the human's `hi`,
/// and an agent whose history is `hi` and the answer `hello`.
async fn fire(m: &dyn Mod, event: ModEvent, haest: &ModEnv) -> Result<(), BoxError> {
    let p = ScriptedProvider::new([]);
    let shared = SharedDeps::new(p.resolver(), CWD, ShutWorkspace);
    let deps = AgentDeps::new(shared, Tools::new(), "");
    let answer = text_answer("hello");
    let history = vec![Message::user("hi"), Message::Assistant(answer.clone())];
    let mut agent = Agent::new(deps, AgentOptions::new("scripted:m").with_history(history));
    let (cancel, c) = (CancellationToken::new(), ctx());
    let call = ModToolCall {
        name: "Echo".into(),
        input: JsonMap::new(),
        read_only: true,
    };
    let (out, ended) = (
        ToolOutput::text("echo"),
        TurnOutcome::new(StopReason::EndTurn),
    );
    match event {
        ModEvent::ToolBefore => m.tool_before(&call, &c, haest).await.map(drop),
        ModEvent::ToolAfter => m.tool_after(&call, &out, &c, haest).await.map(drop),
        ModEvent::PromptSubmit => m.prompt_submit("hi", haest).await.map(drop),
        ModEvent::SystemPrompt => m.system_prompt(haest).await.map(drop),
        ModEvent::ModelBefore => m.model_before(&mut agent, &cancel, haest).await.map(drop),
        ModEvent::ModelAfter => {
            let step = m.model_after(&mut agent, &answer, &cancel, haest).await;
            step.map(drop)
        }
        ModEvent::TurnEnd => m
            .turn_end(&mut agent, &ended, &cancel, haest)
            .await
            .map(drop),
    }
}
