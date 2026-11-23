//! A hook that fails or answers too late, under each `on_error` and its
//! event's default: `block` ends the event (refuses the call, stops the
//! turn, or drops what the hooks did), `continue` reports it and asks the
//! next binding.

use std::time::Duration;

use haest_core::{
    NoHooks, StopReason, Tools,
    error::BoxError,
    tool::{ToolError, input},
};

use super::*;
use crate::{
    hook::{Binding, OnError},
    payload::{
        ModelAfterPayload, ModelBeforePayload, PromptSubmitPayload, SystemPromptPayload,
        ToolAfterPayload, ToolBeforePayload, TurnEndPayload,
    },
};

#[derive(Debug, Clone, Copy)]
enum Fails {
    /// The hook returns an error.
    Error,
    /// The hook outlives its binding's timeout.
    Late,
}

const TIMEOUT: Duration = Duration::from_millis(50);

/// Every way to fail, under every `on_error` (`None`: the event's default).
fn variants() -> Vec<(Fails, Option<OnError>)> {
    let ons = [None, Some(OnError::Block), Some(OnError::Continue)];
    [Fails::Error, Fails::Late]
        .into_iter()
        .flat_map(|f| ons.map(|on| (f, on)))
        .collect()
}

/// A blocked call's or turn's error carries the hook's own error, when
/// the hook gave one.
fn says_why(fails: Fails, error: Option<String>) -> bool {
    match fails {
        Fails::Error => error.is_some_and(|e| e.contains("the hook broke")),
        Fails::Late => error.is_some(),
    }
}

/// Whether `on` blocks, with `E`'s default for `None`.
fn blocks<E: Event>(on: Option<OnError>) -> bool {
    on.unwrap_or(E::ON_ERROR) == OnError::Block
}

/// A hook that fails as told, within [`TIMEOUT`].
fn failing<E: Event>(fails: Fails, on: Option<OnError>) -> Binding<E> {
    let hook = move |_: E, _: CancellationToken| async move {
        match fails {
            Fails::Error => Err::<E::Decision, BoxError>("the hook broke".into()),
            Fails::Late => {
                tokio::time::sleep(Duration::from_secs(30)).await;
                Ok(E::Decision::default())
            }
        }
    };
    let binding = Binding::new(hook).with_timeout(TIMEOUT);
    match on {
        Some(on) => binding.with_on_error(on),
        None => binding,
    }
}

/// A hook that always answers `answer`.
fn answering<E: Event>(answer: Value) -> Binding<E> {
    Binding::new(move |_: E, _: CancellationToken| {
        let answer = answer.clone();
        async move { Ok::<_, BoxError>(E::read_decision(answer)?) }
    })
}

fn with_ui(hooks: Hooks) -> (Mods, Arc<TestUi>) {
    let ui = TestUi::new();
    (Mods::new(env(ui.clone(), WorkspaceMode::Real), [hooks]), ui)
}

#[tokio::test]
async fn a_failed_tool_before_hook_refuses_the_call_unless_it_continues() {
    for (fails, on) in variants() {
        let hooks = Hooks::new()
            .on_tool_before(failing::<ToolBeforePayload>(fails, on))
            .on_tool_before(answering::<ToolBeforePayload>(
                json!({ "input": { "v": "next" } }),
            ));
        let (mods, ui) = with_ui(hooks);
        let echo = Probe::new("Echo");
        let tools = wrapped(mods, Tools::from_iter([echo.clone()]));
        let out = tools[0]
            .execute(input(json!({"v": "hi"})).unwrap(), &ctx())
            .await;
        let case = format!("{fails:?} {on:?}");
        if blocks::<ToolBeforePayload>(on) {
            let why = match out {
                Err(ToolError::Failed(why)) => Some(why),
                other => panic!("{case}: not refused: {other:?}"),
            };
            assert!(says_why(fails, why), "{case}");
            assert!(echo.ran().is_empty(), "{case}");
        } else {
            assert_eq!(out.unwrap().content, "echo:next", "{case}");
        }
        assert_eq!(ui.errors().len(), 1, "{case}: the human sees it");
    }
}

#[tokio::test]
async fn a_failed_tool_after_hook_keeps_the_output_or_the_hooks_before_it() {
    for (fails, on) in variants() {
        let hooks = Hooks::new()
            .on_tool_after(answering::<ToolAfterPayload>(json!({ "content": "first" })))
            .on_tool_after(failing::<ToolAfterPayload>(fails, on));
        let (mods, ui) = with_ui(hooks);
        let tools = wrapped(mods, Tools::from_iter([Probe::new("Echo")]));
        let out = tools[0]
            .execute(input(json!({"v": "hi"})).unwrap(), &ctx())
            .await;
        let kept = if blocks::<ToolAfterPayload>(on) {
            "echo:hi"
        } else {
            "first"
        };
        assert_eq!(out.unwrap().content, kept, "{fails:?} {on:?}");
        assert_eq!(ui.errors().len(), 1, "{fails:?} {on:?}");
    }
}

#[tokio::test]
async fn a_failed_prompt_or_system_hook_drops_or_keeps_what_the_hooks_before_it_did() {
    for (fails, on) in variants() {
        let hooks = Hooks::new()
            .on_prompt_submit(answering::<PromptSubmitPayload>(json!("first")))
            .on_prompt_submit(failing::<PromptSubmitPayload>(fails, on))
            .on_system_prompt(answering::<SystemPromptPayload>(json!("rule")))
            .on_system_prompt(failing::<SystemPromptPayload>(fails, on));
        let (mods, ui) = with_ui(hooks);
        let blocked = blocks::<PromptSubmitPayload>(on);
        let text = mods.rewrite_prompt("hi").await;
        assert_eq!(
            text,
            if blocked { "hi" } else { "first" },
            "{fails:?} {on:?}"
        );
        let added = mods.system_append().await;
        assert_eq!(added, if blocked { "" } else { "rule" }, "{fails:?} {on:?}");
        assert_eq!(ui.errors().len(), 2, "{fails:?} {on:?}");
    }
}

/// One turn of a scripted agent with `hooks`: how it ended, how many
/// model calls it made, how many failures the human saw.
async fn turn(hooks: Hooks) -> (StopReason, Option<String>, usize, usize) {
    let (mods, ui) = with_ui(hooks);
    let p = ScriptedProvider::new(["one", "two", "three"].map(text_answer));
    let end = looped(&p, NoHooks, &mods, Tools::new())
        .run("go", &mut Vec::new())
        .await;
    (
        end.stop_reason,
        end.error,
        p.requests().len(),
        ui.errors().len(),
    )
}

#[tokio::test]
async fn a_failed_model_hook_stops_the_turn_with_its_error_unless_it_continues() {
    for (fails, on) in variants() {
        let case = format!("{fails:?} {on:?}");
        let before =
            turn(Hooks::new().on_model_before(failing::<ModelBeforePayload>(fails, on))).await;
        let after =
            turn(Hooks::new().on_model_after(failing::<ModelAfterPayload>(fails, on))).await;
        if blocks::<ModelBeforePayload>(on) {
            let (stop, error, calls, errors) = before;
            assert_eq!((stop, calls, errors), (StopReason::Error, 0, 0), "{case}");
            assert!(says_why(fails, error), "{case}");
            let (stop, error, calls, _) = after;
            assert_eq!((stop, calls), (StopReason::Error, 1), "{case}");
            assert!(says_why(fails, error), "{case}");
        } else {
            for (stop, error, calls, errors) in [before, after] {
                assert_eq!(
                    (stop, error, calls, errors),
                    (StopReason::EndTurn, None, 1, 1),
                    "{case}"
                );
            }
        }
    }
}

#[tokio::test]
async fn a_failed_turn_end_hook_ends_the_turn_or_lets_the_next_keep_it_going() {
    for (fails, on) in variants() {
        let hooks = Hooks::new()
            .on_turn_end(failing::<TurnEndPayload>(fails, on))
            .on_turn_end(once::<TurnEndPayload>(json!({ "continue": "go on" })));
        let (stop, error, calls, errors) = turn(hooks).await;
        assert_eq!(
            (stop, error),
            (StopReason::EndTurn, None),
            "{fails:?} {on:?}"
        );
        // Blocked, the turn ends at its first answer; passed over, the
        // second hook keeps it going once, and the first fails again.
        let expected = if blocks::<TurnEndPayload>(on) {
            (1, 1)
        } else {
            (2, 2)
        };
        assert_eq!((calls, errors), expected, "{fails:?} {on:?}");
    }
}
