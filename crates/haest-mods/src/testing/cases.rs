//! The decision cases: every answer a hook may give to an event, and what
//! it must have done. A bridge proves it speaks the protocol by taking the
//! same effect as a [`HookSet`] for each one ([`effect`] measures it).

use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use async_trait::async_trait;
use haest_core::{
    Message, NoHooks, Tools,
    error::BoxError,
    testing::{ScriptedProvider, text_answer},
    tool::{JsonMap, ToolError},
};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use super::{Probe, ctx, looped, wrapped};
use crate::{
    env::ModEnv,
    events::{Event, ModEvent},
    hook::{Binding, Hook},
    hook_set::HookSet,
    payload::{
        ModelAfterPayload, ModelBeforePayload, PromptSubmitPayload, SessionView,
        SystemPromptPayload, ToolAfterPayload, ToolBeforePayload, TurnEndPayload,
    },
    registry::Mods,
};

/// A hook every event takes: one value that is a [`Hook`] of each event's
/// payload, as a type with `impl<E: Event> Hook<E>` is.
pub trait AnyHook:
    Hook<ToolBeforePayload>
    + Hook<ToolAfterPayload>
    + Hook<PromptSubmitPayload>
    + Hook<SystemPromptPayload>
    + Hook<ModelBeforePayload>
    + Hook<ModelAfterPayload>
    + Hook<TurnEndPayload>
{
}

impl<H> AnyHook for H where
    H: Hook<ToolBeforePayload>
        + Hook<ToolAfterPayload>
        + Hook<PromptSubmitPayload>
        + Hook<SystemPromptPayload>
        + Hook<ModelBeforePayload>
        + Hook<ModelAfterPayload>
        + Hook<TurnEndPayload>
{
}

/// `hooks` with `hook` bound to `event` too, through the event's own method.
pub fn bind(hooks: HookSet, event: ModEvent, hook: impl AnyHook) -> HookSet {
    match event {
        ModEvent::ToolBefore => hooks.on_tool_before(Binding::<ToolBeforePayload>::new(hook)),
        ModEvent::ToolAfter => hooks.on_tool_after(Binding::<ToolAfterPayload>::new(hook)),
        ModEvent::PromptSubmit => hooks.on_prompt_submit(Binding::<PromptSubmitPayload>::new(hook)),
        ModEvent::SystemPrompt => hooks.on_system_prompt(Binding::<SystemPromptPayload>::new(hook)),
        ModEvent::ModelBefore => hooks.on_model_before(Binding::<ModelBeforePayload>::new(hook)),
        ModEvent::ModelAfter => hooks.on_model_after(Binding::<ModelAfterPayload>::new(hook)),
        ModEvent::TurnEnd => hooks.on_turn_end(Binding::<TurnEndPayload>::new(hook)),
    }
}

/// A hook of any event that answers `answer` (read as its event's
/// decision) the first time it is asked, and passes after ([`once`]).
#[derive(Debug, Clone)]
pub struct Once {
    answer: Value,
    first: Arc<AtomicBool>,
}

#[async_trait]
impl<E: Event> Hook<E> for Once {
    async fn run(&self, _: E, _: &CancellationToken) -> Result<E::Decision, BoxError> {
        if self.first.swap(false, Ordering::SeqCst) {
            Ok(E::read_decision(self.answer.clone())?)
        } else {
            Ok(E::Decision::default())
        }
    }
}

/// A hook that answers `answer` (read as its event's decision) the first
/// time its event fires, and passes after.
pub fn once(answer: Value) -> Once {
    Once {
        answer,
        first: Arc::new(AtomicBool::new(true)),
    }
}

/// A [`HookSet`] answering `answer` once to `event`, bound through the
/// event's own method. Named `hooks` in reports.
pub fn hooks_answering(event: ModEvent, answer: Value) -> HookSet {
    bind(HookSet::new(), event, once(answer))
}

/// A hook of any event that names `session` on `env` as it runs, as a
/// host's /clear may while an event is under way, and passes.
#[derive(Debug, Clone)]
pub struct Naming {
    env: ModEnv,
    session: SessionView,
}

#[async_trait]
impl<E: Event> Hook<E> for Naming {
    async fn run(&self, _: E, _: &CancellationToken) -> Result<E::Decision, BoxError> {
        self.env.set_session(self.session.clone());
        Ok(E::Decision::default())
    }
}

/// A hook of any event that names `session` on `env` as it runs ([`Naming`]).
pub fn naming(env: &ModEnv, session: SessionView) -> Naming {
    Naming {
        env: env.clone(),
        session,
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
/// probe inside the mods (a call a failed hook refused is
/// `{ "failed": true }`, and an `Echo` the mods hide is
/// `{ "hidden": true }`, neither offered nor run), `prompt.submit` rewrites
/// `hi`, `system.prompt` is what the mods append, and a loop event runs a
/// turn on a scripted model (whose first answer is `SUMMARY`, for a
/// compaction) over two earlier exchanges.
pub async fn effect(event: ModEvent, mods: Mods, name: &str) -> Value {
    match event {
        ModEvent::ToolBefore | ModEvent::ToolAfter => {
            let echo = Probe::new("Echo");
            let tools = wrapped(mods, Tools::from_iter([echo.clone()]));
            let Some(offered) = tools.iter().find(|t| t.name() == "Echo") else {
                return json!({ "out": { "hidden": true }, "ran": echo.ran() });
            };
            // A map built directly, not `tool::input(..)`: its `Result`
            // would need an unwrap, and `effect` is public kit code that
            // does not panic.
            let call = JsonMap::from_iter([("v".into(), "hi".into())]);
            let out = match offered.execute(call, &ctx()).await {
                Ok(o) => json!({ "content": o.content, "isError": o.is_error }),
                // A hook that failed refuses the call in words that name its
                // mod and its form: what counts is that it failed.
                Err(ToolError::Failed(_)) => json!({ "failed": true }),
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

/// A tool event's [`effect`] when its hook failed `tool.before`: the call
/// was refused and did not run.
fn failed(v: &Value) -> bool {
    v["out"] == json!({ "failed": true }) && v["ran"] == json!([])
}

/// A loop event's [`effect`] when its hook changed nothing: one model call
/// on the human's `go`, no message added, and no stop.
fn untouched(v: &Value) -> bool {
    v["error"].is_null() && v["asked"] == json!(["go"]) && v["users"] == json!(["a", "b", "go"])
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
        // Answers read by the one decoder (decision/json.rs) whatever
        // carries them: `null` is absent, and a value that is not the
        // key's type is the hook's failure (here: it changes nothing),
        // never coerced to text.
        case(E::ToolBefore, json!({ "deny": null }), |v| {
            v["out"]["content"] == "echo:hi"
        }),
        case(E::ToolBefore, json!({ "deny": 42 }), failed),
        case(E::ToolBefore, json!({ "deny": false }), failed),
        case(E::ToolBefore, json!({ "input": "x" }), failed),
        case(E::PromptSubmit, json!(42), |v| *v == "hi"),
        case(E::SystemPrompt, json!(42), |v| *v == ""),
        // JavaScript's `cond && 'text'`: nothing to add, and no failure.
        case(E::SystemPrompt, json!(false), |v| *v == ""),
        case(E::ModelBefore, json!({ "stop": false }), untouched),
        case(E::ModelBefore, json!({ "steer": 42 }), untouched),
        case(E::ModelBefore, json!({ "compact": "yes" }), untouched),
        case(E::ModelAfter, json!({ "steer": true }), untouched),
        case(E::TurnEnd, json!({ "continue": 1 }), untouched),
    ]
}
