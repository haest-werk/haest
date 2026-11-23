//! Every event Claude Code has, through each transport in the claude
//! dialect: the hook is handed Claude Code's input, and each Claude Code
//! answer takes the effect of the haest decision D42 maps it to.

use haest_core::workspace::WorkspaceMode;
use haest_mods::Mods;
use serde_json::{Value, json};

use super::*;

/// The input Claude Code hands a hook on `event`, in `dir`, as `effect`
/// drives it (an `Echo` call with `{ v: "hi" }`, the prompt `hi`, a turn
/// whose answer is `SUMMARY`).
fn claude_input(event: ModEvent, dir: &Path) -> Value {
    let name = claude_event(event).expect("a Claude Code event");
    let mut input = json!({
        "session_id": "s-1",
        "transcript_path": "/t/s-1.jsonl",
        "cwd": dir.to_string_lossy(),
        "hook_event_name": name,
    });
    let more = match event {
        ModEvent::ToolBefore => json!({ "tool_name": "Echo", "tool_input": { "v": "hi" } }),
        ModEvent::ToolAfter => json!({
            "tool_name": "Echo",
            "tool_input": { "v": "hi" },
            "tool_response": "echo:hi",
        }),
        ModEvent::PromptSubmit => json!({ "prompt": "hi" }),
        ModEvent::TurnEnd => json!({
            "stop_hook_active": false,
            "last_assistant_message": "SUMMARY",
        }),
        _ => json!({}),
    };
    input
        .as_object_mut()
        .unwrap()
        .extend(more.as_object().unwrap().clone());
    input
}

#[tokio::test]
async fn every_claude_code_event_hands_a_shell_and_an_http_hook_claude_codes_input() {
    let claude = Claude::new()
        .with_session_id("s-1")
        .with_transcript_path("/t/s-1.jsonl");
    for via in VIAS {
        for event in ModEvent::ALL {
            if claude_event(*event).is_none() {
                continue;
            }
            let remote = Remote::answering(*via, "").await;
            let mods = Mods::new(
                env(TestUi::new(), WorkspaceMode::Sandbox),
                [remote.claude_hooks(*event, claude.clone())],
            );
            effect(*event, mods, "hooks").await;
            let received = remote.received();
            assert!(!received.is_empty(), "{via:?}: {event} fired");
            assert_eq!(
                received[0],
                claude_input(*event, remote.dir()),
                "{via:?}: {event}"
            );
        }
    }
}

/// What a Claude Code hook answers, and the haest decision it is.
struct Mapped {
    event: ModEvent,
    claude: &'static str,
    haest: Value,
}

fn mapped(event: ModEvent, claude: &'static str, haest: Value) -> Mapped {
    Mapped {
        event,
        claude,
        haest,
    }
}

/// D42's mapping, one row per answer.
fn mapping() -> Vec<Mapped> {
    use ModEvent as E;
    vec![
        mapped(
            E::ToolBefore,
            r#"{"hookSpecificOutput":{"hookEventName":"PreToolUse","permissionDecision":"deny","permissionDecisionReason":"no"}}"#,
            json!({ "deny": "no" }),
        ),
        mapped(
            E::ToolBefore,
            r#"{"decision":"block","reason":"no"}"#,
            json!({ "deny": "no" }),
        ),
        mapped(
            E::ToolBefore,
            r#"{"hookSpecificOutput":{"hookEventName":"PreToolUse","permissionDecision":"ask","permissionDecisionReason":"sure?"}}"#,
            json!({ "deny": "sure?" }),
        ),
        mapped(
            E::ToolBefore,
            r#"{"hookSpecificOutput":{"hookEventName":"PreToolUse","permissionDecision":"allow","updatedInput":{"v":"new"}}}"#,
            json!({ "input": { "v": "new" } }),
        ),
        mapped(
            E::ToolBefore,
            r#"{"hookSpecificOutput":{"hookEventName":"PreToolUse","permissionDecision":"defer"}}"#,
            Value::Null,
        ),
        mapped(E::ToolBefore, "looks fine to me", Value::Null),
        mapped(
            E::ToolAfter,
            r#"{"hookSpecificOutput":{"hookEventName":"PostToolUse","updatedToolOutput":"replaced"}}"#,
            json!({ "content": "replaced" }),
        ),
        mapped(
            E::ToolAfter,
            r#"{"hookSpecificOutput":{"hookEventName":"PostToolUse","additionalContext":"a note"}}"#,
            json!({ "content": "echo:hi\n\na note" }),
        ),
        mapped(
            E::ToolAfter,
            r#"{"decision":"block","reason":"look again"}"#,
            json!({ "content": "echo:hi\n\nlook again" }),
        ),
        mapped(E::PromptSubmit, "be brief", json!("hi\n\nbe brief")),
        mapped(
            E::PromptSubmit,
            r#"{"hookSpecificOutput":{"hookEventName":"UserPromptSubmit","additionalContext":"be brief"}}"#,
            json!("hi\n\nbe brief"),
        ),
        mapped(E::SystemPrompt, "today is Monday", json!("today is Monday")),
        mapped(
            E::SystemPrompt,
            r#"{"hookSpecificOutput":{"hookEventName":"SessionStart","additionalContext":"added"}}"#,
            json!("added"),
        ),
        mapped(
            E::TurnEnd,
            r#"{"decision":"block","reason":"go on"}"#,
            json!({ "continue": "go on" }),
        ),
        mapped(
            E::TurnEnd,
            r#"{"hookSpecificOutput":{"hookEventName":"Stop","additionalContext":"go on"}}"#,
            json!({ "continue": "go on" }),
        ),
        mapped(
            E::TurnEnd,
            r#"{"continue":false,"stopReason":"halt","systemMessage":"bye","suppressOutput":true}"#,
            Value::Null,
        ),
        mapped(E::TurnEnd, "", Value::Null),
    ]
}

#[tokio::test]
async fn a_claude_code_answer_takes_the_effect_of_the_haest_decision_it_maps_to() {
    for Mapped {
        event,
        claude,
        haest,
    } in mapping()
    {
        let from_hooks = effect(
            event,
            Mods::new(
                env(TestUi::new(), WorkspaceMode::Sandbox),
                [hooks_answering(event, haest.clone())],
            ),
            "hooks",
        )
        .await;
        for via in VIAS {
            let remote = Remote::answering(*via, claude).await;
            let mods = Mods::new(
                env(TestUi::new(), WorkspaceMode::Sandbox),
                [remote.claude_hooks(event, Claude::new())],
            );
            let from = effect(event, mods, "hooks").await;
            assert_eq!(from, from_hooks, "{via:?}: {event} answering {claude}");
        }
    }
}

#[tokio::test]
async fn a_refused_prompt_is_a_failure_the_human_sees_since_haest_cannot_refuse_one() {
    for via in VIAS {
        let remote = Remote::answering(*via, r#"{"decision":"block","reason":"not today"}"#).await;
        let ui = TestUi::new();
        let mods = Mods::new(
            env(ui.clone(), WorkspaceMode::Sandbox),
            [remote.claude_hooks(ModEvent::PromptSubmit, Claude::new())],
        );
        assert_eq!(mods.rewrite_prompt("hi").await, "hi", "{via:?}");
        let errors = ui.errors().join("\n");
        assert!(
            errors.contains("refused the prompt") && errors.contains("not today"),
            "{via:?}: {errors}"
        );
    }
}
