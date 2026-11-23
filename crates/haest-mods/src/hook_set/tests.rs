//! [`Hooks`](super::Hooks): every decision takes the same effect from a
//! `.ts` mod and from a `Hooks` closure (one table, the testing module's
//! decision cases); what a failed or late hook does under each `on_error`
//! (on_error.rs); order, first wins, matchers and interrupts (order.rs);
//! the parity guard (parity.rs).

use std::sync::Arc;

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
