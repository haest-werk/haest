//! The decision cases: every answer a hook may give to an event, and what
//! it must have done. A bridge proves it speaks the protocol by taking the
//! same effect as [`Hooks`] for each one ([`effect`] measures it).

use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use haest_core::{
    Message, NoHooks, Tools,
    error::BoxError,
    testing::{ScriptedProvider, text_answer},
};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use super::{Probe, ctx, input, looped, wrapped};
use crate::{
    events::{Event, ModEvent},
    hook::Hook,
    hook_set::Hooks,
    registry::Mods,
};

/// A hook that answers `answer` (read as its event's decision) the first
/// time its event fires, and passes after.
pub fn once<E: Event>(answer: Value) -> impl Hook<E> {
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

/// [`Hooks`] answering `answer` once to `event`, bound through the event's
/// own method. Named `hooks` in reports.
pub fn hooks_answering(event: ModEvent, answer: Value) -> Hooks {
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

/// The text of a user message.
pub fn user_text(m: &Message) -> Option<String> {
    match m {
        Message::User(u) => Some(u.content.clone()),
        _ => None,
    }
}

/// What `mods` did when `event` fired, in words that do not name the mod
/// (`name` is how it signs a stopped turn): a tool event runs an `Echo`
/// probe inside the mods, `prompt.submit` rewrites `hi`, `system.prompt`
/// is what the mods append, and a loop event runs a turn on a scripted
/// model (whose first answer is `SUMMARY`, for a compaction) over two
/// earlier exchanges.
pub async fn effect(event: ModEvent, mods: Mods, name: &str) -> Value {
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

/// One answer a hook may give to an event, and how to tell from its
/// [`effect`] that it took effect.
#[derive(Debug, Clone)]
pub struct DecisionCase {
    /// The event.
    pub event: ModEvent,
    /// What the hook answers, as JSON.
    pub answer: Value,
    /// Whether an [`effect`] shows the answer did what it says.
    pub took_effect: fn(&Value) -> bool,
}

fn has(v: &Value, text: &str) -> bool {
    v.to_string().contains(text)
}

fn case(event: ModEvent, answer: Value, took_effect: fn(&Value) -> bool) -> DecisionCase {
    DecisionCase {
        event,
        answer,
        took_effect,
    }
}

/// Every answer a handler may give, by event, and what it must have done.
pub fn decision_cases() -> Vec<DecisionCase> {
    use ModEvent as E;
    vec![
        case(E::ToolBefore, json!({ "deny": "no" }), |v| {
            has(&v["out"]["refused"], "no") && v["ran"] == json!([])
        }),
        case(E::ToolBefore, json!({ "input": { "v": "new" } }), |v| {
            v["out"]["content"] == "echo:new"
        }),
        case(E::ToolBefore, Value::Null, |v| {
            v["out"]["content"] == "echo:hi"
        }),
        case(
            E::ToolAfter,
            json!({ "content": "replaced", "isError": true }),
            |v| v["out"] == json!({ "content": "replaced", "isError": true }),
        ),
        case(E::ToolAfter, Value::Null, |v| {
            v["out"]["content"] == "echo:hi"
        }),
        case(E::PromptSubmit, json!("rewritten"), |v| *v == "rewritten"),
        case(E::PromptSubmit, Value::Null, |v| *v == "hi"),
        case(E::SystemPrompt, json!("added"), |v| *v == "added"),
        case(E::ModelBefore, json!({ "stop": "halt" }), |v| {
            has(&v["error"], "halt") && v["asked"] == json!([])
        }),
        case(E::ModelBefore, json!({ "steer": "steered" }), |v| {
            has(&v["users"], "steered")
        }),
        case(
            E::ModelBefore,
            json!({ "compact": { "instructions": "keep paths" } }),
            |v| has(&v["asked"][0], "keep paths") && has(&v["users"][0], "SUMMARY"),
        ),
        case(E::ModelAfter, json!({ "stop": "halt" }), |v| {
            has(&v["error"], "halt") && v["asked"].as_array().is_some_and(|a| a.len() == 1)
        }),
        case(E::ModelAfter, json!({ "steer": "cite" }), |v| {
            has(&v["users"], "cite")
        }),
        case(E::TurnEnd, json!({ "continue": "go on" }), |v| {
            has(&v["users"], "go on") && v["asked"].as_array().is_some_and(|a| a.len() == 2)
        }),
        case(E::TurnEnd, json!({ "compact": true }), |v| {
            v["error"].is_null() && v["asked"].as_array().is_some_and(|a| a.len() == 2)
        }),
    ]
}
