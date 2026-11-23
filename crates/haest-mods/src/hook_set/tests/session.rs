//! The session a hook is handed: the one the conversation's env names as of
//! each event, on every event, through a nested [`Mods`] too, and left out
//! of the JSON when none is named.

use std::sync::Mutex;

use async_trait::async_trait;

use super::*;
use crate::{
    api::ModRef,
    payload::{SessionView, SystemPromptPayload},
};

/// What [`Noting`] writes down of one payload: the session's id, and the
/// payload JSON's `session`.
type Noted = (Option<String>, Value);

/// A hook of any event that writes down the session each payload names,
/// as its JSON says it, and passes.
#[derive(Clone, Default)]
struct Noting(Arc<Mutex<Vec<Noted>>>);

#[async_trait]
impl<E: Event> Hook<E> for Noting {
    async fn run(&self, payload: E, _: &CancellationToken) -> Result<E::Decision, BoxError> {
        let id = payload.session().map(|s| s.id.clone());
        let json = serde_json::to_value(&payload)?["session"].take();
        self.0.lock().unwrap().push((id, json));
        Ok(E::Decision::default())
    }
}

impl Noting {
    /// Each payload's session id, in order, and forget them.
    fn ids(&self) -> Vec<Option<String>> {
        let mut seen = self.0.lock().unwrap();
        seen.drain(..).map(|(id, _)| id).collect()
    }
}

/// `hook` bound to `event`.
fn bound(event: ModEvent, hook: Noting) -> Hooks {
    let hooks = Hooks::new();
    match event {
        ModEvent::ToolBefore => hooks.on_tool_before(hook),
        ModEvent::ToolAfter => hooks.on_tool_after(hook),
        ModEvent::PromptSubmit => hooks.on_prompt_submit(hook),
        ModEvent::SystemPrompt => hooks.on_system_prompt(hook),
        ModEvent::ModelBefore => hooks.on_model_before(hook),
        ModEvent::ModelAfter => hooks.on_model_after(hook),
        ModEvent::TurnEnd => hooks.on_turn_end(hook),
    }
}

fn session(n: u8) -> SessionView {
    SessionView::new(format!("s-{n}")).with_transcript_path(format!("/t/s-{n}.jsonl"))
}

#[tokio::test]
async fn every_event_hands_a_hook_the_session_the_env_names_as_of_that_event() {
    for event in ModEvent::ALL {
        let noting = Noting::default();
        let haest = env(TestUi::new(), WorkspaceMode::Sandbox).with_session(session(1));
        let mods = Mods::new(haest, [bound(*event, noting.clone())]);
        effect(*event, mods.clone(), "hooks").await;
        let first = noting.ids();
        assert!(!first.is_empty(), "{event} fired");
        assert!(
            first.iter().all(|id| id.as_deref() == Some("s-1")),
            "{event}: {first:?}"
        );

        // The host's /clear: the hook, bound once, gets the new session.
        mods.env().set_session(session(2));
        effect(*event, mods, "hooks").await;
        let second = noting.ids();
        assert!(
            second.iter().all(|id| id.as_deref() == Some("s-2")),
            "{event}: {second:?}"
        );
    }
}

#[tokio::test]
async fn the_session_is_json_only_when_the_host_names_one() {
    let noting = Noting::default();
    let mods = Mods::new(
        env(TestUi::new(), WorkspaceMode::Sandbox),
        [Hooks::new().on_system_prompt(noting.clone())],
    );
    mods.system_append().await;
    mods.env().set_session(session(1));
    mods.system_append().await;
    mods.env().set_session(SessionView::new("s-2"));
    mods.system_append().await;
    let json: Vec<Value> = noting
        .0
        .lock()
        .unwrap()
        .iter()
        .map(|(_, j)| j.clone())
        .collect();
    assert_eq!(
        json,
        [
            Value::Null,
            json!({ "id": "s-1", "transcriptPath": "/t/s-1.jsonl" }),
            json!({ "id": "s-2" }),
        ]
    );
    // A payload built outside `Hooks` names none: its JSON is as it was.
    assert_eq!(
        serde_json::to_value(SystemPromptPayload::new()).unwrap(),
        json!({})
    );
}

#[tokio::test]
async fn a_hook_in_a_nested_mods_on_its_own_env_gets_the_outer_session() {
    let noting = Noting::default();
    let inner = Mods::new(
        env(TestUi::new(), WorkspaceMode::Sandbox),
        [Hooks::new().on_system_prompt(noting.clone())],
    );
    let outer = env(TestUi::new(), WorkspaceMode::Sandbox).with_session(session(1));
    let mods = Mods::new(outer, [ModRef::new(inner)]);
    mods.system_append().await;
    mods.env().set_session(session(2));
    mods.system_append().await;
    assert_eq!(noting.ids(), [Some("s-1".into()), Some("s-2".into())]);
}
