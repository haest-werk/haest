//! The protocol's payloads are the JSON a TypeScript handler receives: per
//! event, the typed payload (haest-mods') writes the words the mod files
//! have always been handed (written out here as they were before the
//! types), reads them back, and a `.ts` handler receives exactly them, plus
//! `session` when the env names one (session_tests.rs has the rest). And
//! every decision a `.ts` handler returns takes the effect the same
//! decision takes from a `HookSet` closure (haest-mods' decision cases).

use haest_core::{
    Agent, AgentDeps, AgentOptions, Effort, Message, NoHooks, SharedDeps, StopReason, ToolOutput,
    Tools, TurnOutcome,
    error::BoxError,
    testing::{ScriptedProvider, text_answer},
    tool::{ToolContext, input},
    workspace::{ShutWorkspace, WorkspaceMode},
};
use haest_mods::{
    Event, HookSet, Mod, ModEvent, ModRef, ModToolCall, ModelAfterPayload, ModelBeforePayload,
    Mods, PromptSubmitPayload, SessionView, SystemPromptPayload, ToolAfterPayload,
    ToolBeforePayload, TurnEndDecision, TurnEndPayload,
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
/// event's `(call, [output,] ctx, session)` with the mod file's own
/// `ctx.signal` left out, `prompt.submit`'s `(text, session)`,
/// `system.prompt`'s `(session)`, and a loop event's one view. A session
/// the host does not name is `undefined`, `null` in the arguments' JSON:
/// left out, as the payload's JSON leaves it out.
fn handed(event: ModEvent, args: &Value) -> Value {
    let arg = |i: usize| args[i].clone();
    let ctx = |i: usize| {
        let mut c = arg(i);
        c.as_object_mut().unwrap().remove("signal");
        c
    };
    let (mut payload, session) = match event {
        ModEvent::ToolBefore => (json!({ "call": arg(0), "ctx": ctx(1) }), arg(2)),
        ModEvent::ToolAfter => (
            json!({ "call": arg(0), "output": arg(1), "ctx": ctx(2) }),
            arg(3),
        ),
        ModEvent::PromptSubmit => (json!({ "text": arg(0) }), arg(1)),
        ModEvent::SystemPrompt => (json!({}), arg(0)),
        ModEvent::ModelBefore | ModEvent::ModelAfter | ModEvent::TurnEnd => return arg(0),
        // `ModEvent` is non-exhaustive outside haest-mods: a new event fails
        // the tests below at run time, as every event is driven through.
        other => panic!("{other} is not driven through a mod file here"),
    };
    if !session.is_null() {
        payload["session"] = session;
    }
    payload
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
        input: input(json!({ "v": "hi" })).unwrap(),
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
    let view = json!({
        "modelSpec": "scripted:m", "effort": "high", "historyLength": 2,
        "contextTokens": answer.usage.context_tokens(), "contextWindow": agent.context_window(),
    });
    let with = |key: &str, v: Value| {
        let mut out = view.clone();
        out[key] = v;
        out
    };
    let cancel = CancellationToken::new();
    let mut ended = TurnOutcome::new(StopReason::EndTurn);
    ended.error = Some("a cap".into());

    // Every event with no session named, then with one: the handler gets
    // the payload's `session` as a `HookSet` binding's payload carries it.
    // The typed payloads are built bare (no session), as a host builds one.
    let named = SessionView::new("s-1").with_transcript_path("/t/s-1.jsonl");
    for session in [None, Some(named)] {
        if let Some(s) = &session {
            haest.set_session(s.clone());
        }
        for event in ModEvent::ALL {
            let mut expected = match *event {
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
                    expected["answer"] = light_answer(&answer);
                    expected["continued"] = json!(false);
                    writes(
                        &TurnEndPayload::new(&agent, &ended, &cancel, &haest),
                        &expected,
                    );
                    m.turn_end(&mut agent, &ended, &cancel, &haest)
                        .await
                        .unwrap();
                    expected
                }
                other => panic!("{other} is not driven through a mod file here"),
            };
            if session.is_some() {
                expected["session"] = json!({ "id": "s-1", "transcriptPath": "/t/s-1.jsonl" });
            }
            let infos = ui.infos();
            let seen: Value = parse(infos.last().expect("the handler ran"));
            assert_eq!(handed(*event, &seen), expected, "{event}, {session:?}");
        }
    }
    assert_eq!(ui.infos().len(), 2 * ModEvent::ALL.len());
}

/// `turn.end`'s `answer` and `continued` are computed once, in haest-mods:
/// a `.ts` handler and a Rust hook in the same loop are handed the same,
/// turn after turn, while a third hook keeps each turn going once.
#[tokio::test]
async fn a_ts_mod_and_a_rust_hook_see_the_same_answer_and_continued() {
    let ui = TestUi::new();
    let haest = env(ui.clone(), WorkspaceMode::Sandbox);
    const SEEN_TS: &str = "export default function (haest: any): void {
      haest.on('turn.end', (turn: any): void => {
        haest.notice(JSON.stringify({ answer: turn.answer, continued: turn.continued }))
      })
    }";
    let ts = js("seen.ts", SEEN_TS, &haest).await.expect("the mod loads");
    let rust_seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::<Value>::new()));
    let log = rust_seen.clone();
    let rust = HookSet::new().on_turn_end(move |p: TurnEndPayload, _: CancellationToken| {
        log.lock()
            .unwrap()
            .push(json!({ "answer": p.answer, "continued": p.continued }));
        async { Ok::<_, BoxError>(TurnEndDecision::default()) }
    });
    let once = HookSet::new().on_turn_end(|p: TurnEndPayload, _: CancellationToken| async move {
        let decision = TurnEndDecision::default();
        Ok::<_, BoxError>(if p.continued {
            decision
        } else {
            decision.with_continue("check once more")
        })
    });
    let mods = Mods::new(
        haest,
        [ModRef::new(ts), ModRef::new(rust), ModRef::new(once)],
    );
    let p = ScriptedProvider::new(["one", "two", "three", "four"].map(text_answer));
    let mut agent = looped(&p, NoHooks, &mods, Tools::new());
    for input in ["go", "again"] {
        assert_eq!(agent.run(input, &mut Vec::new()).await.error, None);
    }

    let from_ts: Vec<Value> = ui.infos().iter().map(|line| parse(line)).collect();
    let from_rust = rust_seen.lock().unwrap().clone();
    assert_eq!(from_ts, from_rust);
    let flags: Vec<&Value> = from_rust.iter().map(|v| &v["continued"]).collect();
    assert_eq!(flags, [false, true, false, true]);
    let said: Vec<&Value> = from_rust
        .iter()
        .map(|v| &v["answer"]["content"][0]["text"])
        .collect();
    assert_eq!(said, ["one", "two", "three", "four"]);
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

/// Every case that differs is listed, not only the first.
#[tokio::test]
async fn a_decision_takes_the_same_effect_from_a_ts_mod_and_a_hooks_closure() {
    let mut differ = Vec::new();
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
        if from_ts != from_hooks || !took_effect(&from_ts) {
            differ.push(format!(
                "{event} answering {answer}:\n  ts:    {from_ts}\n  hooks: {from_hooks}"
            ));
        }
    }
    assert!(differ.is_empty(), "{}", differ.join("\n"));
}
