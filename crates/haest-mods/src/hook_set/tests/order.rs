//! One event's bindings: run in the order added, rewrites chain, the first
//! deny, stop or continue wins, every steer is kept and the first compaction
//! counts; a matcher picks the tools; an interrupt is no failure.

use std::sync::Mutex;

use haest_core::{StopReason, ToolOutput, tool::ToolError};

use super::*;
use crate::{
    api::Before,
    decision::{Compact, ModelBeforeDecision, TurnEndDecision},
    hook::{Binding, OnError},
    payload::{
        ModelAfterPayload, ModelBeforePayload, PromptSubmitPayload, ToolAfterPayload,
        ToolBeforePayload, TurnEndPayload,
    },
};

type Log = Arc<Mutex<Vec<String>>>;

fn log() -> Log {
    Arc::default()
}

fn note(log: &Log, line: impl Into<String>) {
    log.lock().unwrap().push(line.into());
}

fn noted(log: &Log) -> Vec<String> {
    log.lock().unwrap().clone()
}

/// A tool.before hook that notes `tag` and answers `then(input v)`.
fn before(log: &Log, tag: &'static str, then: fn(&str) -> Before) -> impl Hook<ToolBeforePayload> {
    let log = log.clone();
    move |p: ToolBeforePayload, _: CancellationToken| {
        note(&log, tag);
        let v = p
            .call
            .input
            .get("v")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        async move { Ok::<_, BoxError>(then(&v)) }
    }
}

fn v(text: String) -> Before {
    Before::Input(input(json!({ "v": text })))
}

#[tokio::test]
async fn bindings_run_in_order_rewrites_chain_and_the_first_deny_wins() {
    let log = log();
    let hooks = Hooks::new()
        .on_tool_before(before(&log, "1", |x| v(format!("{x}a"))))
        .on_tool_before(before(&log, "2", |x| v(format!("{x}b"))));
    let mods = Mods::new(env(TestUi::new(), WorkspaceMode::Real), [hooks.clone()]);
    let tools = wrapped(mods, Tools::from_iter([Probe::new("Echo")]));
    let out = tools[0].execute(input(json!({"v": "x"})), &ctx()).await;
    assert_eq!(out.unwrap().content, "echo:xab");
    assert_eq!(noted(&log), ["1", "2"]);

    let refusing = hooks
        .on_tool_before(before(&log, "3", |_| Before::Deny("stop here".into())))
        .on_tool_before(before(&log, "4", |_| Before::Pass));
    let mods = Mods::new(env(TestUi::new(), WorkspaceMode::Real), [refusing]);
    let echo = Probe::new("Echo");
    let tools = wrapped(mods, Tools::from_iter([echo.clone()]));
    match tools[0].execute(input(json!({"v": "x"})), &ctx()).await {
        Err(ToolError::Input(why)) => assert!(why.contains("stop here")),
        other => panic!("not refused: {other:?}"),
    }
    assert_eq!(
        noted(&log)[2..],
        ["1", "2", "3"],
        "the fourth is never asked"
    );
    assert!(echo.ran().is_empty());
}

#[tokio::test]
async fn output_and_prompt_rewrites_chain_and_system_texts_join_in_order() {
    let shout = |p: ToolAfterPayload, _: CancellationToken| async move {
        let out = ToolOutput::text(p.output.content.to_uppercase());
        Ok::<_, BoxError>(Some(out))
    };
    let bang = |p: ToolAfterPayload, _: CancellationToken| async move {
        Ok::<_, BoxError>(Some(ToolOutput::text(format!("{}!", p.output.content))))
    };
    let add = |tail: &'static str| {
        move |p: PromptSubmitPayload, _: CancellationToken| async move {
            Ok::<_, BoxError>(Some(format!("{} {tail}", p.text)))
        }
    };
    let says = |text: &'static str| {
        move |_: crate::SystemPromptPayload, _: CancellationToken| async move {
            Ok::<_, BoxError>(Some(text.to_string()))
        }
    };
    let hooks = Hooks::new()
        .on_tool_after(shout)
        .on_tool_after(bang)
        .on_prompt_submit(add("a"))
        .on_prompt_submit(add("b"))
        .on_system_prompt(says("one"))
        .on_system_prompt(says(""))
        .on_system_prompt(says("two"));
    let mods = Mods::new(env(TestUi::new(), WorkspaceMode::Real), [hooks]);
    assert_eq!(mods.rewrite_prompt("hi").await, "hi a b");
    let added = mods.system_append().await;
    assert!(added.find("one") < added.find("two"), "{added}");
    let tools = wrapped(mods, Tools::from_iter([Probe::new("Echo")]));
    let out = tools[0].execute(input(json!({"v": "hi"})), &ctx()).await;
    assert_eq!(out.unwrap().content, "ECHO:HI!");
}

#[tokio::test]
async fn in_a_loop_event_steers_are_kept_and_the_first_stop_or_continue_wins() {
    let log = log();
    let asked = |log: &Log, tag: &'static str| {
        let log = log.clone();
        move |p: TurnEndPayload, _: CancellationToken| {
            note(&log, format!("{tag} {}", p.agent.history_length));
            // go, a, the steers, b: the turn's first end; then c.
            let first = p.agent.history_length < 5;
            async move {
                let go_on = TurnEndDecision::default().with_continue(format!("go on, {tag}"));
                Ok::<_, BoxError>(if first {
                    go_on
                } else {
                    TurnEndDecision::default()
                })
            }
        }
    };
    let hooks = Hooks::new()
        .on_model_after(once::<ModelAfterPayload>(json!({ "steer": "one" })))
        .on_model_after(once::<ModelAfterPayload>(json!({ "steer": "two" })))
        .on_turn_end(asked(&log, "first"))
        .on_turn_end(asked(&log, "second"));
    let mods = Mods::new(env(TestUi::new(), WorkspaceMode::Real), [hooks]);
    let p = ScriptedProvider::new(["a", "b", "c", "d"].map(text_answer));
    let mut agent = looped(&p, NoHooks, &mods, Tools::new());
    let end = agent.run("go", &mut Vec::new()).await;
    assert_eq!(end.error, None);
    let users: Vec<String> = agent.history().iter().filter_map(user_text).collect();
    let said = users.join("|");
    assert!(said.find("one") < said.find("two"), "{said}");
    assert!(
        said.contains("go on, first") && !said.contains("go on, second"),
        "{said}"
    );
    let asked_second: Vec<String> = noted(&log)
        .into_iter()
        .filter(|l| l.starts_with("second"))
        .collect();
    assert_eq!(
        asked_second.len(),
        1,
        "asked only once the first let the turn end: {:?}",
        noted(&log)
    );

    let stops = Hooks::new()
        .on_model_before(|_: ModelBeforePayload, _: CancellationToken| async {
            Ok::<_, BoxError>(ModelBeforeDecision::default().with_stop("halt"))
        })
        .on_model_before(Binding::new(
            |_: ModelBeforePayload, _: CancellationToken| async {
                Err::<ModelBeforeDecision, BoxError>("never asked".into())
            },
        ));
    let ui = TestUi::new();
    let mods = Mods::new(env(ui.clone(), WorkspaceMode::Real), [stops]);
    let p = ScriptedProvider::new([text_answer("a")]);
    let end = looped(&p, NoHooks, &mods, Tools::new())
        .run("go", &mut Vec::new())
        .await;
    assert_eq!(end.stop_reason, StopReason::Error);
    assert!(end.error.is_some_and(|e| e.contains("halt")));
    assert!(ui.errors().is_empty(), "the second binding was never asked");
}

#[tokio::test]
async fn the_first_compaction_asked_for_counts() {
    let compacts = |instructions: &'static str| {
        move |p: ModelBeforePayload, _: CancellationToken| async move {
            let asked = Compact::new().with_instructions(instructions);
            let decision = ModelBeforeDecision::default();
            Ok::<_, BoxError>(if p.agent.history_length > 4 {
                decision.with_compact(asked)
            } else {
                decision
            })
        }
    };
    let hooks = Hooks::new()
        .on_model_before(compacts("keep the paths"))
        .on_model_before(compacts("keep the dates"));
    let mods = Mods::new(env(TestUi::new(), WorkspaceMode::Real), [hooks]);
    let p = ScriptedProvider::new([text_answer("SUMMARY"), text_answer("done")]);
    let mut agent = looped(&p, NoHooks, &mods, Tools::new());
    let earlier = ["a", "b"].map(|t| [Message::user(t), Message::Assistant(text_answer(t))]);
    agent.replace_history(earlier.concat());
    let end = agent.run("c", &mut Vec::new()).await;
    assert_eq!(end.error, None);
    let requests = p.requests();
    assert_eq!(requests.len(), 2, "one summary, one answer");
    let summary = requests[0].messages.last().and_then(user_text).unwrap();
    assert!(summary.contains("keep the paths") && !summary.contains("keep the dates"));
}

#[tokio::test]
async fn a_matcher_asks_a_hook_only_about_the_tools_it_names() {
    let deny = |why: &'static str| {
        move |_: ToolBeforePayload, _: CancellationToken| async move {
            Ok::<_, BoxError>(Before::Deny(why.into()))
        }
    };
    let seen = |_: ToolAfterPayload, _: CancellationToken| async {
        Ok::<_, BoxError>(Some(ToolOutput::text("seen")))
    };
    let hooks = Hooks::new()
        .on_tool_before(Binding::new(deny("no bash")).with_matcher("Bash"))
        .on_tool_before(Binding::new(deny("no github")).with_matcher("mcp__github__*"))
        .on_tool_after(Binding::new(seen).with_matcher("Echo"));
    let mods = Mods::new(env(TestUi::new(), WorkspaceMode::Real), [hooks]);
    let names = [
        "Bash",
        "Echo",
        "mcp__github__search",
        "mcp__gitlab__search",
        "BashOutput",
    ];
    let tools = wrapped(mods, names.map(Probe::new));
    let mut got = Vec::new();
    for tool in &tools {
        got.push(match tool.execute(input(json!({"v": "x"})), &ctx()).await {
            Ok(out) => out.content,
            Err(e) => e.to_string(),
        });
    }
    assert!(got[0].contains("no bash"), "{got:?}");
    assert_eq!(got[1], "seen");
    assert!(got[2].contains("no github"), "{got:?}");
    assert_eq!(got[3..], ["mcp__gitlab__search:x", "bashoutput:x"]);
}

#[tokio::test]
async fn an_interrupt_while_a_hook_decides_is_no_failure_whatever_its_on_error() {
    for on in [OnError::Block, OnError::Continue] {
        let waits = |_: ToolBeforePayload, _: CancellationToken| async {
            std::future::pending::<Result<Before, BoxError>>().await
        };
        let ui = TestUi::new();
        let hooks = Hooks::new().on_tool_before(Binding::new(waits).with_on_error(on));
        let mods = Mods::new(env(ui.clone(), WorkspaceMode::Real), [hooks]);
        let echo = Probe::new("Echo");
        let tools = wrapped(mods, Tools::from_iter([echo.clone()]));
        let c = ctx();
        let cancel = c.cancel().clone();
        let (out, ()) = tokio::join!(tools[0].execute(input(json!({})), &c), async {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            cancel.cancel();
        });
        assert!(matches!(out, Err(ToolError::Input(_))), "{on}: {out:?}");
        assert!(echo.ran().is_empty() && ui.errors().is_empty(), "{on}");
    }
}
