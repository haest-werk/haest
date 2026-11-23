//! The protocol's payloads are the JSON a TypeScript handler receives: per
//! event, the typed payload (haest-mods') writes the words the mod files
//! have always been handed (written out here as they were before the
//! types), reads them back, and a `.ts` handler receives exactly them. And
//! every decision a `.ts` handler returns takes the effect the same
//! decision takes from a `Hooks` closure (haest-mods' decision cases).

use haest_core::{
    Agent, AgentDeps, AgentOptions, Effort, Message, SharedDeps, StopReason, ToolOutput, Tools,
    TurnOutcome,
    testing::{ScriptedProvider, text_answer},
    tool::ToolContext,
    workspace::{ShutWorkspace, WorkspaceMode},
};
use haest_mods::{
    Event, Mod, ModEvent, ModToolCall, ModelAfterPayload, ModelBeforePayload, Mods,
    PromptSubmitPayload, SystemPromptPayload, ToolAfterPayload, ToolBeforePayload, TurnEndPayload,
};
use serde::{Serialize, de::DeserializeOwned};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use crate::testkit::*;

/// A `.ts` mod whose every handler writes down what it was handed.
fn recorder() -> String {
    let on_every: String = ModEvent::WORDS
        .iter()
        .map(|e| format!("  haest.on('{e}', (...args: unknown[]): void => {{ haest.notice(JSON.stringify(args)) }})\n"))
        .collect();
    format!("export default function (haest: any): void {{\n{on_every}}}\n")
}

/// `payload` writes `expected`, and reads back as itself.
fn writes<E: Event + PartialEq>(payload: &E, expected: &Value) {
    assert_eq!(
        &serde_json::to_value(payload).unwrap(),
        expected,
        "{}",
        E::NAME
    );
    let back: E = serde_json::from_value(expected.clone()).unwrap();
    assert_eq!(&back, payload, "{}", E::NAME);
}

/// A handler's arguments as the payload they were handed in: a tool
/// event's `(call, [output,] ctx)` with the mod file's own `ctx.signal`
/// left out, `prompt.submit`'s `(text)`, `system.prompt`'s `()`, and a loop
/// event's one view.
fn handed(event: ModEvent, args: &Value) -> Value {
    let arg = |i: usize| args[i].clone();
    let ctx = |i: usize| {
        let mut c = arg(i);
        c.as_object_mut().unwrap().remove("signal");
        c
    };
    match event {
        ModEvent::ToolBefore => json!({ "call": arg(0), "ctx": ctx(1) }),
        ModEvent::ToolAfter => json!({ "call": arg(0), "output": arg(1), "ctx": ctx(2) }),
        ModEvent::PromptSubmit => json!({ "text": arg(0) }),
        ModEvent::SystemPrompt => json!({}),
        ModEvent::ModelBefore | ModEvent::ModelAfter | ModEvent::TurnEnd => arg(0),
        // `ModEvent` is non-exhaustive outside haest-core: a new event fails
        // the tests below at run time, as every event is driven through.
        other => panic!("{other} is not driven through a mod file here"),
    }
}

fn light_answer(answer: &impl Serialize) -> Value {
    let mut v = serde_json::to_value(answer).unwrap();
    v.as_object_mut().unwrap().remove("providerData");
    v
}

fn parse<T: DeserializeOwned>(s: &str) -> T {
    serde_json::from_str(s).unwrap()
}

#[tokio::test]
async fn every_payload_is_the_json_a_ts_handler_receives() {
    let ui = TestUi::new();
    let haest = env(ui.clone(), WorkspaceMode::Sandbox);
    let m = js("recorder.ts", &recorder(), &haest)
        .await
        .expect("the mod loads");

    let call = ModToolCall {
        name: "Echo".into(),
        input: input(json!({ "v": "hi" })),
        read_only: true,
    };
    let ctx = ToolContext::new(CWD, ShutWorkspace).with_tool_call_id("t1");
    let ctx_json = json!({ "cwd": CWD, "toolCallId": "t1" });
    let call_json = json!({ "name": "Echo", "input": { "v": "hi" }, "readOnly": true });
    let output = ToolOutput::text("echo:hi");
    let answer = text_answer("hello");
    let p = ScriptedProvider::new([]);
    let deps = AgentDeps::new(
        SharedDeps::new(p.resolver(), CWD, ShutWorkspace),
        Tools::new(),
        "",
    );
    let history = vec![Message::user("hi"), Message::Assistant(answer.clone())];
    let opts = AgentOptions::new("scripted:m")
        .with_effort(Effort::High)
        .with_history(history.clone());
    let mut agent = Agent::new(deps, opts);
    let view = json!({ "modelSpec": "scripted:m", "effort": "high", "historyLength": 2 });
    let with = |key: &str, v: Value| {
        let mut out = view.clone();
        out[key] = v;
        out
    };
    let cancel = CancellationToken::new();
    let mut ended = TurnOutcome::new(StopReason::EndTurn);
    ended.error = Some("a cap".into());

    for event in ModEvent::ALL {
        let expected = match *event {
            ModEvent::ToolBefore => {
                let expected = json!({ "call": call_json, "ctx": ctx_json });
                writes(&ToolBeforePayload::new(&call, &ctx), &expected);
                m.tool_before(&call, &ctx, &haest).await.unwrap();
                expected
            }
            ModEvent::ToolAfter => {
                let out = serde_json::to_value(&output).unwrap();
                let expected = json!({ "call": call_json, "output": out, "ctx": ctx_json });
                writes(&ToolAfterPayload::new(&call, &output, &ctx), &expected);
                m.tool_after(&call, &output, &ctx, &haest).await.unwrap();
                expected
            }
            ModEvent::PromptSubmit => {
                let expected = json!({ "text": "hi" });
                writes(&PromptSubmitPayload::new("hi"), &expected);
                m.prompt_submit("hi", &haest).await.unwrap();
                expected
            }
            ModEvent::SystemPrompt => {
                let expected = json!({});
                writes(&SystemPromptPayload::new(), &expected);
                m.system_prompt(&haest).await.unwrap();
                expected
            }
            ModEvent::ModelBefore => {
                let expected = with("lastMessage", light_answer(&history[1]));
                writes(&ModelBeforePayload::new(&agent), &expected);
                m.model_before(&mut agent, &cancel, &haest).await.unwrap();
                expected
            }
            ModEvent::ModelAfter => {
                let expected = with("answer", light_answer(&answer));
                writes(&ModelAfterPayload::new(&agent, &answer), &expected);
                m.model_after(&mut agent, &answer, &cancel, &haest)
                    .await
                    .unwrap();
                expected
            }
            ModEvent::TurnEnd => {
                let mut expected = with("stopReason", json!("end_turn"));
                expected["error"] = json!("a cap");
                writes(&TurnEndPayload::new(&agent, &ended), &expected);
                m.turn_end(&mut agent, &ended, &cancel, &haest)
                    .await
                    .unwrap();
                expected
            }
            other => panic!("{other} is not driven through a mod file here"),
        };
        let infos = ui.infos();
        let seen: Value = parse(infos.last().expect("the handler ran"));
        assert_eq!(handed(*event, &seen), expected, "{event}");
    }
    assert_eq!(ui.infos().len(), ModEvent::ALL.len());
}

/// A `.ts` mod that answers `answer` the first time `event` fires.
fn ts_answering(event: ModEvent, answer: &Value) -> String {
    format!(
        "export default function (haest: any) {{
           let first = true
           haest.on('{event}', () => {{ if (!first) return undefined; first = false; return {answer} }})
         }}"
    )
}

#[tokio::test]
async fn a_decision_takes_the_same_effect_from_a_ts_mod_and_a_hooks_closure() {
    for DecisionCase {
        event,
        answer,
        took_effect,
    } in decision_cases()
    {
        let haest = env(TestUi::new(), WorkspaceMode::Sandbox);
        let file = js("same.ts", &ts_answering(event, &answer), &haest)
            .await
            .expect("the mod loads");
        let from_ts = effect(event, Mods::new(haest.clone(), [file]), "same.ts").await;
        let from_hooks = effect(
            event,
            Mods::new(haest, [hooks_answering(event, answer.clone())]),
            "hooks",
        )
        .await;
        assert_eq!(from_ts, from_hooks, "{event} answering {answer}");
        assert!(
            took_effect(&from_ts),
            "{event} answering {answer}: {from_ts}"
        );
    }
}
