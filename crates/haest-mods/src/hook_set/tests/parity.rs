//! PARITY: every event has its `Hooks` binding, and its `Mod` event on
//! `Hooks` runs what is bound there.

use std::sync::Mutex;

use haest_core::{
    Agent, AgentDeps, AgentOptions, SharedDeps, StopReason, ToolOutput, TurnOutcome, tool::JsonMap,
    workspace::ShutWorkspace,
};

use super::*;
use crate::{
    Mod, ModToolCall,
    payload::{
        ModelAfterPayload, ModelBeforePayload, PromptSubmitPayload, SystemPromptPayload,
        ToolAfterPayload, ToolBeforePayload, TurnEndPayload,
    },
};

type Log = Arc<Mutex<Vec<&'static str>>>;

/// A hook that notes its event's name and passes.
fn noting<E: Event>(log: &Log) -> impl Hook<E> {
    let log = log.clone();
    move |_: E, _: CancellationToken| {
        log.lock().unwrap().push(E::NAME.as_str());
        async { Ok::<_, BoxError>(E::Decision::default()) }
    }
}

/// A new event fails to compile below until it is bound through its own
/// `on_*` method (which its row in events.rs writes) and fired through its
/// `Mod` method; `impl Mod for Hooks` names every method, so a `Mod` event
/// `Hooks` does not fire fails the gates there.
#[tokio::test]
async fn every_event_has_a_binding_and_its_mod_event_runs_it() {
    let log = Log::default();
    let mut hooks = Hooks::new();
    for event in ModEvent::ALL {
        hooks = match *event {
            ModEvent::ToolBefore => hooks.on_tool_before(noting::<ToolBeforePayload>(&log)),
            ModEvent::ToolAfter => hooks.on_tool_after(noting::<ToolAfterPayload>(&log)),
            ModEvent::PromptSubmit => hooks.on_prompt_submit(noting::<PromptSubmitPayload>(&log)),
            ModEvent::SystemPrompt => hooks.on_system_prompt(noting::<SystemPromptPayload>(&log)),
            ModEvent::ModelBefore => hooks.on_model_before(noting::<ModelBeforePayload>(&log)),
            ModEvent::ModelAfter => hooks.on_model_after(noting::<ModelAfterPayload>(&log)),
            ModEvent::TurnEnd => hooks.on_turn_end(noting::<TurnEndPayload>(&log)),
        };
    }
    let haest = env(TestUi::new(), WorkspaceMode::Sandbox);
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
    for event in ModEvent::ALL {
        let ran: Result<(), BoxError> = match *event {
            ModEvent::ToolBefore => hooks.tool_before(&call, &c, &haest).await.map(drop),
            ModEvent::ToolAfter => hooks.tool_after(&call, &out, &c, &haest).await.map(drop),
            ModEvent::PromptSubmit => hooks.prompt_submit("hi", &haest).await.map(drop),
            ModEvent::SystemPrompt => hooks.system_prompt(&haest).await.map(drop),
            ModEvent::ModelBefore => {
                let step = hooks.model_before(&mut agent, &cancel, &haest).await;
                step.map(drop)
            }
            ModEvent::ModelAfter => {
                let step = hooks
                    .model_after(&mut agent, &answer, &cancel, &haest)
                    .await;
                step.map(drop)
            }
            ModEvent::TurnEnd => {
                let end = hooks.turn_end(&mut agent, &ended, &cancel, &haest).await;
                end.map(drop)
            }
        };
        ran.unwrap();
    }
    assert_eq!(*log.lock().unwrap(), ModEvent::WORDS);
}
