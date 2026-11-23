//! [`Hooks`](super::Hooks): every decision takes the same effect from a
//! `.ts` mod and from a `Hooks` closure (one table, below); what a failed
//! or late hook does under each `on_error` (on_error.rs); order, first
//! wins, matchers and interrupts (order.rs); the parity guard (parity.rs).

use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use haest_core::{
    Message, NoHooks, Tools,
    error::BoxError,
    testing::{ScriptedProvider, text_answer},
    workspace::WorkspaceMode,
};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use crate::{
    events::{Event, ModEvent},
    hook::Hook,
    hook_set::Hooks,
    registry::Mods,
    testkit::*,
};

mod on_error;
mod order;
mod parity;

/// A hook that answers `answer` (read as its event's decision) the first
/// time its event fires, and passes after.
fn once<E: Event>(answer: Value) -> impl Hook<E> {
    let first = Arc::new(AtomicBool::new(true));
    move |_: E, _: CancellationToken| {
        let given = first.swap(false, Ordering::SeqCst).then(|| answer.clone());
        async move {
            Ok::<_, BoxError>(match given {
                Some(v) => serde_json::from_value::<E::Decision>(v)?,
                None => E::Decision::default(),
            })
        }
    }
}

/// `Hooks` answering `answer` once to `event`, bound through the event's
/// own method.
fn bound(event: ModEvent, answer: Value) -> Hooks {
    let hooks = Hooks::new();
    match event {
        ModEvent::ToolBefore => hooks.on_tool_before(once(answer)),
        ModEvent::ToolAfter => hooks.on_tool_after(once(answer)),
        ModEvent::PromptSubmit => hooks.on_prompt_submit(once(answer)),
        ModEvent::SystemPrompt => hooks.on_system_prompt(once(answer)),
        ModEvent::ModelBefore => hooks.on_model_before(once(answer)),
        ModEvent::ModelAfter => hooks.on_model_after(once(answer)),
        ModEvent::TurnEnd => hooks.on_turn_end(once(answer)),
    }
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

fn user_text(m: &Message) -> Option<String> {
    match m {
        Message::User(u) => Some(u.content.clone()),
        _ => None,
    }
}

/// What `mods` did when `event` fired, in words that do not name the mod
/// (`name` is how it signs a stopped turn).
async fn effect(event: ModEvent, mods: Mods, name: &str) -> Value {
    match event {
        ModEvent::ToolBefore | ModEvent::ToolAfter => {
            let echo = Probe::new("Echo");
            let tools = wrapped(mods, Tools::from_iter([echo.clone()]));
            let out = match tools[0].execute(input(json!({"v": "hi"})), &ctx()).await {
                Ok(o) => json!({ "content": o.content, "isError": o.is_error }),
                Err(e) => json!({ "refused": e.to_string() }),
            };
            json!({ "out": out, "ran": echo.ran() })
        }
        ModEvent::PromptSubmit => json!(mods.rewrite_prompt("hi").await),
        ModEvent::SystemPrompt => json!(mods.system_append().await),
        ModEvent::ModelBefore | ModEvent::ModelAfter | ModEvent::TurnEnd => {
            let p = ScriptedProvider::new(["SUMMARY", "one", "two", "three"].map(text_answer));
            let mut agent = looped(&p, NoHooks, &mods, Tools::new());
            let earlier =
                ["a", "b"].map(|t| [Message::user(t), Message::Assistant(text_answer(t))]);
            agent.replace_history(earlier.concat());
            let end = agent.run("go", &mut Vec::new()).await;
            let users: Vec<String> = agent.history().iter().filter_map(user_text).collect();
            let asked: Vec<Option<String>> = p
                .requests()
                .iter()
                .map(|r| r.messages.last().and_then(user_text))
                .collect();
            let error = end.error.map(|e| e.replace(name, "<mod>"));
            json!({ "stopReason": end.stop_reason, "error": error, "users": users, "asked": asked })
        }
    }
}

type Means = fn(&Value) -> bool;

fn has(v: &Value, text: &str) -> bool {
    v.to_string().contains(text)
}

/// Every answer a handler may give, by event, and what it must have done.
fn cases() -> Vec<(ModEvent, Value, Means)> {
    use ModEvent as E;
    vec![
        (E::ToolBefore, json!({ "deny": "no" }), |v| {
            has(&v["out"]["refused"], "no") && v["ran"] == json!([])
        }),
        (E::ToolBefore, json!({ "input": { "v": "new" } }), |v| {
            v["out"]["content"] == "echo:new"
        }),
        (E::ToolBefore, Value::Null, |v| {
            v["out"]["content"] == "echo:hi"
        }),
        (
            E::ToolAfter,
            json!({ "content": "replaced", "isError": true }),
            |v| v["out"] == json!({ "content": "replaced", "isError": true }),
        ),
        (E::ToolAfter, Value::Null, |v| {
            v["out"]["content"] == "echo:hi"
        }),
        (E::PromptSubmit, json!("rewritten"), |v| *v == "rewritten"),
        (E::PromptSubmit, Value::Null, |v| *v == "hi"),
        (E::SystemPrompt, json!("added"), |v| *v == "added"),
        (E::ModelBefore, json!({ "stop": "halt" }), |v| {
            has(&v["error"], "halt") && v["asked"] == json!([])
        }),
        (E::ModelBefore, json!({ "steer": "steered" }), |v| {
            has(&v["users"], "steered")
        }),
        (
            E::ModelBefore,
            json!({ "compact": { "instructions": "keep paths" } }),
            |v| has(&v["asked"][0], "keep paths") && has(&v["users"][0], "SUMMARY"),
        ),
        (E::ModelAfter, json!({ "stop": "halt" }), |v| {
            has(&v["error"], "halt") && v["asked"].as_array().is_some_and(|a| a.len() == 1)
        }),
        (E::ModelAfter, json!({ "steer": "cite" }), |v| {
            has(&v["users"], "cite")
        }),
        (E::TurnEnd, json!({ "continue": "go on" }), |v| {
            has(&v["users"], "go on") && v["asked"].as_array().is_some_and(|a| a.len() == 2)
        }),
        (E::TurnEnd, json!({ "compact": true }), |v| {
            v["error"].is_null() && v["asked"].as_array().is_some_and(|a| a.len() == 2)
        }),
    ]
}

#[tokio::test]
async fn a_decision_takes_the_same_effect_from_a_ts_mod_and_a_hooks_closure() {
    for (event, answer, means) in cases() {
        let haest = env(TestUi::new(), WorkspaceMode::Sandbox);
        let file = js("same.ts", &ts_answering(event, &answer), &haest)
            .await
            .expect("the mod loads");
        let from_ts = effect(event, Mods::new(haest.clone(), [file]), "same.ts").await;
        let from_hooks = effect(
            event,
            Mods::new(haest, [bound(event, answer.clone())]),
            "hooks",
        )
        .await;
        assert_eq!(from_ts, from_hooks, "{event} answering {answer}");
        assert!(means(&from_ts), "{event} answering {answer}: {from_ts}");
    }
}
