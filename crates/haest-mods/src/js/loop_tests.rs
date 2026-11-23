//! A mod file's loop events and tool changes: every Rust event has its
//! TypeScript twin, stop / steer / continue / compact work from a `.ts` mod,
//! and its tools come and go as it says.

use haest_core::{
    Agent, AgentDeps, AgentOptions, Message, NoHooks, SharedDeps, StopReason, Tools, TurnOutcome,
    testing::{ScriptedProvider, text_answer, tool_answer},
    tool::{JsonMap, ToolSource},
    workspace::{ShutWorkspace, WorkspaceMode},
};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use crate::{
    Before, Mod, ModToolCall, ModTools, Mods, Step, TurnEnd, events::ModEvent,
    registry::CommandOutcome, testkit::*,
};

/// `source` loaded as the `.ts` mod `file`, alone in its [`Mods`].
async fn ts_mods(ui: &std::sync::Arc<TestUi>, file: &str, source: &str) -> Mods {
    let haest = env(ui.clone(), WorkspaceMode::Sandbox);
    let m = js(file, source, &haest).await.expect("the mod loads");
    Mods::new(haest, [m])
}

fn user_text(m: &Message) -> Option<&str> {
    match m {
        Message::User(u) => Some(&u.content),
        _ => None,
    }
}

/// TWIN PARITY. A `.ts` mod with a handler on every event of the Rust list
/// (events.rs) hears each one when the matching `Mod` method runs on it. A
/// `Mod` event without a twin fails the gates at `impl Mod for JsMod`
/// (every method named), an event the list has and bootstrap.js does not
/// handle stops every mod file loading, and a new event fails to compile
/// below until it is driven here.
#[tokio::test]
async fn every_mod_event_reaches_its_handler_in_a_ts_mod() {
    let on_every: String = ModEvent::WORDS
        .iter()
        .map(|e| format!("  haest.on('{e}', (): void => {{ haest.notice('{e}') }})\n"))
        .collect();
    let source = format!("export default function (haest: any): void {{\n{on_every}}}\n");
    let ui = TestUi::new();
    let haest = env(ui.clone(), WorkspaceMode::Sandbox);
    let m = js("every.ts", &source, &haest)
        .await
        .expect("the mod loads");
    let p = ScriptedProvider::new([]);
    let deps = AgentDeps::new(
        SharedDeps::new(p.resolver(), CWD, ShutWorkspace),
        Tools::new(),
        "",
    );
    let answer = text_answer("hello");
    let history = vec![Message::user("hi"), Message::Assistant(answer.clone())];
    let mut agent = Agent::new(deps, AgentOptions::new("scripted:m").with_history(history));
    let (cancel, c) = (CancellationToken::new(), ctx());
    let call = ModToolCall {
        name: "Echo".into(),
        input: JsonMap::new(),
        read_only: true,
    };
    let output = haest_core::ToolOutput::text("echo");
    let ended = TurnOutcome::new(StopReason::EndTurn);

    for event in ModEvent::ALL {
        match *event {
            ModEvent::ToolBefore => assert_eq!(
                m.tool_before(&call, &c, &haest).await.unwrap(),
                Before::Pass
            ),
            ModEvent::ToolAfter => assert_eq!(
                m.tool_after(&call, &output, &c, &haest).await.unwrap(),
                None
            ),
            ModEvent::PromptSubmit => {
                assert_eq!(m.prompt_submit("hi", &haest).await.unwrap(), None)
            }
            ModEvent::SystemPrompt => assert_eq!(m.system_prompt(&haest).await.unwrap(), None),
            ModEvent::ModelBefore => {
                assert_eq!(
                    m.model_before(&mut agent, &cancel, &haest).await.unwrap(),
                    Step::Pass
                );
            }
            ModEvent::ModelAfter => {
                let step = m.model_after(&mut agent, &answer, &cancel, &haest).await;
                assert_eq!(step.unwrap(), Step::Pass);
            }
            ModEvent::TurnEnd => {
                let end = m.turn_end(&mut agent, &ended, &cancel, &haest).await;
                assert_eq!(end.unwrap(), TurnEnd::Pass);
            }
        }
    }
    assert_eq!(ui.infos(), ModEvent::WORDS);
}

#[tokio::test]
async fn the_loop_events_see_the_model_the_history_length_and_the_last_message_only() {
    let source = r#"export default function (haest: any) {
      for (const e of ['model.before', 'model.after', 'turn.end'])
        haest.on(e, (v: unknown) => { haest.notice(JSON.stringify(v)) })
    }"#;
    let ui = TestUi::new();
    let mods = ts_mods(&ui, "views.ts", source).await;
    let p = ScriptedProvider::new([text_answer("hello")]);
    let _ = looped(&p, NoHooks, &mods, Tools::new())
        .run("hi", &mut Vec::new())
        .await;

    let seen: Vec<Value> = ui
        .infos()
        .iter()
        .map(|s| serde_json::from_str(s).unwrap())
        .collect();
    let [before, after, end] = &seen[..] else {
        panic!("{seen:?}")
    };
    assert_eq!(
        (&before["modelSpec"], &before["effort"]),
        (&json!("scripted:m"), &Value::Null)
    );
    assert_eq!(before["historyLength"], 1);
    assert_eq!(
        (
            &before["lastMessage"]["role"],
            &before["lastMessage"]["content"]
        ),
        (&json!("user"), &json!("hi"))
    );
    assert_eq!(after["historyLength"], 2);
    assert_eq!(after["answer"]["content"][0]["text"], "hello");
    assert_eq!(
        (&end["stopReason"], &end["error"]),
        (&json!("end_turn"), &Value::Null)
    );
}

#[tokio::test]
async fn a_ts_mod_stops_a_turn_after_an_answer_and_before_a_model_call() {
    let source = r#"export default function (haest: any) {
      haest.on('model.after', (v: any) =>
        v.answer.content.some((p: any) => p.type === 'tool_call' && p.name === 'Delete') ? { stop: 'no deleting' } : undefined)
      haest.on('model.before', (v: any) => v.historyLength > 2 ? { stop: 'enough for today' } : undefined)
    }"#;
    let mods = ts_mods(&TestUi::new(), "stops.ts", source).await;
    let p = ScriptedProvider::new([tool_answer(&[("d1", "Delete", json!({}))])]);
    let delete = Probe::new("Delete");
    let mut agent = looped(&p, NoHooks, &mods, Tools::from_iter([delete.clone()]));

    let first = agent.run("tidy up", &mut Vec::new()).await;
    assert_eq!(first.stop_reason, StopReason::Error);
    assert!(first.error.unwrap_or_default().contains("no deleting"));
    assert!(delete.ran().is_empty());

    let second = agent.run("go on", &mut Vec::new()).await;
    assert!(
        second
            .error
            .unwrap_or_default()
            .contains("enough for today")
    );
    assert_eq!(p.requests().len(), 1, "no model call after the stop");
}

#[tokio::test]
async fn a_ts_mod_steers_after_an_answer_and_keeps_a_turn_going_at_its_end() {
    let source = r#"export default function (haest: any) {
      let reminded = false
      haest.on('model.after', () => reminded ? undefined : (reminded = true, { steer: 'cite your sources' }))
      haest.on('turn.end', (v: any) => v.historyLength < 6 ? { continue: 'go on' } : undefined)
    }"#;
    let mods = ts_mods(&TestUi::new(), "steers.ts", source).await;
    let p = ScriptedProvider::new(["one", "two", "three"].map(text_answer));
    let mut agent = looped(&p, NoHooks, &mods, Tools::new());

    let end = agent.run("go", &mut Vec::new()).await;

    assert_eq!(
        (end.stop_reason, p.requests().len()),
        (StopReason::EndTurn, 3)
    );
    let users: Vec<_> = agent.history().iter().filter_map(user_text).collect();
    assert_eq!(users, ["go", "cite your sources", "go on"]);
}

#[tokio::test]
async fn a_ts_mod_compacts_before_a_model_call_with_its_instructions_and_as_a_turn_ends() {
    let source = r#"export default function (haest: any) {
      haest.on('model.before', (v: any) => v.historyLength > 4 ? { compact: { instructions: 'keep the file paths' } } : undefined)
    }"#;
    let mods = ts_mods(&TestUi::new(), "compacts.ts", source).await;
    let p = ScriptedProvider::new([text_answer("SUMMARY"), text_answer("done")]);
    let mut agent = looped(&p, NoHooks, &mods, Tools::new());
    let earlier = ["a", "b"].map(|t| [Message::user(t), Message::Assistant(text_answer(t))]);
    agent.replace_history(earlier.concat());
    let end = agent.run("c", &mut Vec::new()).await;
    assert_eq!(end.error, None);
    let [summary, turn] = &p.requests()[..] else {
        panic!("two calls")
    };
    assert!(
        summary
            .messages
            .last()
            .and_then(user_text)
            .unwrap()
            .contains("keep the file paths")
    );
    assert!(user_text(&turn.messages[0]).unwrap().contains("SUMMARY"));

    let at_end = "export default (haest: any) => haest.on('turn.end', () => ({ compact: true }))";
    let mods = ts_mods(&TestUi::new(), "at-end.ts", at_end).await;
    let p = ScriptedProvider::new([text_answer("done"), text_answer("SUMMARY")]);
    let mut agent = looped(&p, NoHooks, &mods, Tools::new());
    let end = agent.run("go", &mut Vec::new()).await;
    assert_eq!(end.error, None);
    assert!(user_text(&agent.history()[0]).unwrap().contains("SUMMARY"));
}

#[tokio::test]
async fn a_throwing_loop_handler_is_reported_and_the_turn_goes_on() {
    let source = r#"export default function (haest: any) {
      haest.on('model.before', () => { throw new Error('boom') })
      haest.on('turn.end', async () => { throw new Error('bust') })
    }"#;
    let ui = TestUi::new();
    let mods = ts_mods(&ui, "throws.ts", source).await;
    let p = ScriptedProvider::new([text_answer("done")]);
    let end = looped(&p, NoHooks, &mods, Tools::new())
        .run("go", &mut Vec::new())
        .await;
    assert_eq!((end.stop_reason, end.error), (StopReason::EndTurn, None));
    let errors = ui.errors();
    assert_eq!(errors.len(), 2, "{errors:?}");
    assert!(errors.iter().all(|e| e.contains("throws.ts")));
}

const TOOLS_TS: &str = r#"export default function (haest: any) {
  const add = (name: string, description: string) => haest.tool({ name, description, execute: () => name })
  add('A', 'v1'); add('B', 'v1'); add('C', 'v1')
  haest.command('hide', (name: string) => { haest.disableTool(name) })
  haest.command('show', (name: string) => { haest.enableTool(name) })
  haest.command('drop', (name: string) => { haest.notice(String(haest.removeTool(name))) })
  haest.command('again', () => { add('B', 'v2') })
}"#;

#[tokio::test]
async fn a_hidden_tool_is_neither_offered_nor_callable_and_comes_back_where_it_was() {
    let mods = ts_mods(&TestUi::new(), "tools.ts", TOOLS_TS).await;
    let source = ModTools::new(Tools::from_iter([Probe::new("Bash")]), mods.clone());
    assert_eq!(names(&source.tools()), ["Bash", "A", "B", "C"]);

    for hidden in ["Bash", "B"] {
        assert_eq!(mods.command("hide", hidden).await, CommandOutcome::Handled);
    }
    assert_eq!(names(&source.tools()), ["A", "C"]);
    assert_eq!(names(&source.callable()), ["A", "C"]);

    for shown in ["B", "Bash"] {
        assert_eq!(mods.command("show", shown).await, CommandOutcome::Handled);
    }
    assert_eq!(names(&source.tools()), ["Bash", "A", "B", "C"]);
}

#[tokio::test]
async fn remove_tool_drops_only_the_mods_own_tool_and_a_replaced_tool_keeps_its_place() {
    let ui = TestUi::new();
    let mods = ts_mods(&ui, "tools.ts", TOOLS_TS).await;
    let source = ModTools::new(Tools::from_iter([Probe::new("Bash")]), mods.clone());

    mods.command("again", "").await;
    let tools = source.tools();
    assert_eq!(names(&tools), ["Bash", "A", "B", "C"]);
    assert_eq!(find(&tools, "B").description(), "v2");

    for name in ["Bash", "B"] {
        mods.command("drop", name).await;
    }
    assert_eq!(ui.infos(), ["false", "true"]);
    assert_eq!(names(&source.tools()), ["Bash", "A", "C"]);
    assert_eq!(names(&source.callable()), ["Bash", "A", "C"]);
}
