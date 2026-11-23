//! The session a mod file's handler is handed: the one the conversation's
//! env names as the event begins, on every event, as its payload's
//! `session` (`{ id, transcriptPath }`, the JSON a `HookSet` binding's payload
//! carries and a shell or HTTP hook in haest's dialect reads), `undefined`
//! when the host names none. A handler handed its payload spread gets it as
//! the last argument; a loop event's one view has it as a field.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use haest_core::{
    NoHooks, Tools,
    error::BoxError,
    testing::{ScriptedProvider, text_answer},
    tool::input,
    workspace::WorkspaceMode,
};
use haest_mods::{Event, Hook, HookSet, ModEvent, ModRef, Mods, SessionView};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use crate::testkit::*;

/// The session `s-<n>`, its transcript beside it.
fn session(n: u8) -> SessionView {
    SessionView::new(format!("s-{n}")).with_transcript_path(format!("/t/s-{n}.jsonl"))
}

/// The session `s-<n>` as a payload's JSON writes it.
fn session_json(n: u8) -> Value {
    json!({ "id": format!("s-{n}"), "transcriptPath": format!("/t/s-{n}.jsonl") })
}

/// How a handler of `event` is handed its session: its parameters, and
/// the expression that reads the session from them.
fn session_of(event: ModEvent) -> (&'static str, &'static str) {
    match event {
        ModEvent::ToolBefore => ("_call: unknown, _ctx: unknown, s: unknown", "s"),
        ModEvent::ToolAfter => (
            "_call: unknown, _output: unknown, _ctx: unknown, s: unknown",
            "s",
        ),
        ModEvent::PromptSubmit => ("_text: unknown, s: unknown", "s"),
        ModEvent::SystemPrompt => ("s: unknown", "s"),
        ModEvent::ModelBefore | ModEvent::ModelAfter | ModEvent::TurnEnd => {
            ("view: any", "view.session")
        }
        // `ModEvent` is non-exhaustive outside haest-mods: a new event fails
        // here, as every event is driven through.
        other => panic!("{other}: say how its handler is handed the session"),
    }
}

/// A `.ts` mod whose handlers on `events` notice `{ event, session }`: the
/// session each was handed, or the text `undefined`.
fn noticing(events: &[ModEvent]) -> String {
    let on: String = events
        .iter()
        .map(|e| {
            let (params, session) = session_of(*e);
            format!("  haest.on('{e}', ({params}): void => seen('{e}', {session}))\n")
        })
        .collect();
    format!(
        "export default function (haest: any): void {{
  const seen = (event: string, session: unknown): void =>
    haest.notice(JSON.stringify({{ event, session: session === undefined ? 'undefined' : session }}))
{on}}}
"
    )
}

/// What the `.ts` mod noticed, in order.
fn noticed(ui: &TestUi) -> Vec<Value> {
    let infos = ui.infos();
    infos
        .iter()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

/// A Rust hook on any event that writes down `{ event, session }` as the
/// `.ts` mod notices it: its payload JSON's `session`, or `undefined`.
#[derive(Clone, Default)]
struct Noting(Arc<Mutex<Vec<Value>>>);

#[async_trait]
impl<E: Event> Hook<E> for Noting {
    async fn run(&self, payload: E, _: &CancellationToken) -> Result<E::Decision, BoxError> {
        let session = match serde_json::to_value(&payload)?["session"].take() {
            Value::Null => json!("undefined"),
            named => named,
        };
        let seen = json!({ "event": E::NAME.as_str(), "session": session });
        self.0.lock().unwrap().push(seen);
        Ok(E::Decision::default())
    }
}

/// On every event, a `.ts` handler and a `HookSet` binding are handed the
/// same session: `undefined` (never `null`) when the host names none, then
/// s-1, then the host's new one after a /clear.
#[tokio::test]
async fn a_ts_handler_gets_the_session_a_hooks_binding_gets_and_undefined_when_none() {
    let ui = TestUi::new();
    let haest = env(ui.clone(), WorkspaceMode::Sandbox);
    let events = ModEvent::ALL;
    let ts = js("session.ts", &noticing(events), &haest)
        .await
        .expect("the mod loads");
    let rust = Noting::default();
    let hooks = events
        .iter()
        .fold(HookSet::new(), |hooks, e| bind(hooks, *e, rust.clone()));
    let mods = Mods::new(haest, [ModRef::new(ts), ModRef::new(hooks)]);
    let tools = wrapped(mods.clone(), Tools::from_iter([Probe::new("Echo")]));
    let p = ScriptedProvider::new(["one", "two", "three"].map(text_answer));
    let mut agent = looped(&p, NoHooks, &mods, Tools::new());

    // None named, then s-1, then the host's /clear: s-2. Each round fires
    // every event once, in the order `ModEvent::ALL` lists them.
    for named in [None, Some(session(1)), Some(session(2))] {
        if let Some(s) = named {
            mods.env().set_session(s);
        }
        let call = input(json!({ "v": "hi" })).unwrap();
        tools[0].execute(call, &ctx()).await.unwrap();
        mods.rewrite_prompt("hi").await;
        mods.system_append().await;
        assert_eq!(agent.run("go", &mut Vec::new()).await.error, None);
    }

    let each = |session: Value| {
        events
            .iter()
            .map(|e| json!({ "event": e.as_str(), "session": session }))
            .collect::<Vec<_>>()
    };
    let expected = [
        each(json!("undefined")),
        each(session_json(1)),
        each(session_json(2)),
    ]
    .concat();
    assert_eq!(noticed(&ui), expected, "the .ts handlers");
    assert_eq!(*rust.0.lock().unwrap(), expected, "the HookSet bindings");
}

#[tokio::test]
async fn every_event_hands_a_ts_handler_the_session_the_env_names_as_of_that_event() {
    for event in ModEvent::ALL {
        let ui = TestUi::new();
        let haest = env(ui.clone(), WorkspaceMode::Sandbox).with_session(session(1));
        let file = js("session.ts", &noticing(&[*event]), &haest)
            .await
            .expect("the mod loads");
        let mods = Mods::new(haest, [file]);
        effect(*event, mods.clone(), "session.ts").await;
        let first = noticed(&ui).len();
        assert!(first > 0, "{event} fired");

        // The host's /clear: the file, loaded once, is handed the new one.
        mods.env().set_session(session(2));
        effect(*event, mods, "session.ts").await;
        let all = noticed(&ui);
        assert!(all.len() > first, "{event} fired again");
        let sessions: Vec<&Value> = all.iter().map(|seen| &seen["session"]).collect();
        let (before, after) = sessions.split_at(first);
        assert!(
            before.iter().all(|s| **s == session_json(1)),
            "{event}: {before:?}"
        );
        assert!(
            after.iter().all(|s| **s == session_json(2)),
            "{event}: {after:?}"
        );
    }
}

/// A `.ts` file, then a `HookSet` whose hook names a new session (the
/// host's /clear while the event runs), then a `HookSet` binding: on every
/// event, the file's handler and the binding after the /clear are both
/// handed the session the event began on.
#[tokio::test]
async fn every_mod_of_one_event_gets_the_session_named_as_that_event_began() {
    for event in ModEvent::ALL {
        let ui = TestUi::new();
        let haest = env(ui.clone(), WorkspaceMode::Sandbox).with_session(session(1));
        let file = js("session.ts", &noticing(&[*event]), &haest)
            .await
            .expect("the mod loads");
        let rust = Noting::default();
        let mods = Mods::new(
            haest.clone(),
            [
                ModRef::new(file),
                ModRef::new(bind(HookSet::new(), *event, naming(&haest, session(2)))),
                ModRef::new(bind(HookSet::new(), *event, rust.clone())),
            ],
        );
        effect(*event, mods, "session.ts").await;
        let seen = [json!({ "event": event.as_str(), "session": session_json(1) })];
        assert_eq!(noticed(&ui), seen, "{event}: the .ts handler");
        assert_eq!(*rust.0.lock().unwrap(), seen, "{event}: the binding");
    }
}
