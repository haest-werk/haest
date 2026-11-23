//! Every event through each transport in haest's dialect: a hook in
//! another process is handed the event's name and the very payload a Rust
//! hook gets, and each of its answers takes the effect the same answer
//! takes from a `Hooks` closure. Both tests drive every event of
//! haest-mods, so an event added without shell and HTTP coverage fails
//! them ([`bound`]).

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use haest_core::{error::BoxError, workspace::WorkspaceMode};
use haest_mods::{Event, Hook, ModRef, Mods};
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use super::*;

/// A Rust hook on any event that writes down each payload's JSON and passes.
#[derive(Clone)]
struct Noting(Arc<Mutex<Vec<Value>>>);

#[async_trait]
impl<E: Event> Hook<E> for Noting {
    async fn run(&self, payload: E, _: &CancellationToken) -> Result<E::Decision, BoxError> {
        self.0.lock().unwrap().push(serde_json::to_value(&payload)?);
        Ok(E::Decision::default())
    }
}

#[tokio::test]
async fn every_event_hands_a_shell_and_an_http_hook_its_name_and_the_payload_a_rust_hook_gets() {
    for via in VIAS {
        for event in ModEvent::ALL {
            let remote = Remote::answering(*via, "").await;
            let rust = Noting(Arc::default());
            let mods = Mods::new(
                env(TestUi::new(), WorkspaceMode::Sandbox),
                [
                    ModRef::new(bound(*event, rust.clone())),
                    ModRef::new(remote.hooks(*event)),
                ],
            );
            effect(*event, mods, "hooks").await;

            let expected: Vec<Value> = rust
                .0
                .lock()
                .unwrap()
                .iter()
                .map(|payload| {
                    let mut input = serde_json::Map::new();
                    input.insert("event".into(), event.as_str().into());
                    input.extend(payload.as_object().unwrap().clone());
                    Value::Object(input)
                })
                .collect();
            assert!(!expected.is_empty(), "{via:?}: {event} fired");
            assert_eq!(remote.received(), expected, "{via:?}: {event}");
        }
    }
}

/// A decision as a hook in haest's dialect prints it: a pass is nothing.
fn printed(answer: &Value) -> String {
    if answer.is_null() {
        String::new()
    } else {
        answer.to_string()
    }
}

#[tokio::test]
async fn every_decision_takes_the_same_effect_from_a_shell_hook_an_http_hook_and_a_closure() {
    for DecisionCase {
        event,
        answer,
        took_effect,
    } in decision_cases()
    {
        let from_hooks = effect(
            event,
            Mods::new(
                env(TestUi::new(), WorkspaceMode::Sandbox),
                [hooks_answering(event, answer.clone())],
            ),
            "hooks",
        )
        .await;
        for via in VIAS {
            let remote = Remote::answering(*via, &printed(&answer)).await;
            let mods = Mods::new(
                env(TestUi::new(), WorkspaceMode::Sandbox),
                [remote.hooks(event)],
            );
            let from = effect(event, mods, "hooks").await;
            assert_eq!(from, from_hooks, "{via:?}: {event} answering {answer}");
            assert!(
                took_effect(&from),
                "{via:?}: {event} answering {answer}: {from}"
            );
        }
    }
}
