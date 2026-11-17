use std::{path::Path, sync::Arc, time::Duration};

use haest_core::{Tools, tool::ToolError, workspace::WorkspaceMode};
use serde_json::json;

use super::{JsMod, ModCaps, ModLoadError};
use crate::{
    api::Decision,
    registry::{CommandOutcome, Mods},
    testkit::*,
};

const GUARD_TS: &str = r#"
interface Call { name: string; input: Record<string, unknown>; readOnly: boolean }
type Verdict = { deny: string } | undefined
const DANGER: RegExp = /rm -rf \//
export default function (haest: { on(event: string, fn: (call: Call) => Verdict): void }): void {
  haest.on('tool.before', (call: Call): Verdict =>
    call.name === 'Bash' && DANGER.test(String(call.input.command)) ? { deny: 'not on my watch' } : undefined)
}
"#;

/// `mods` with one file mod loaded from `source`.
async fn with_mod(ui: Arc<TestUi>, file: &str, source: &str) -> Mods {
    let haest = env(ui, WorkspaceMode::Sandbox);
    let m = js(file, source, &haest).await.expect("the mod loads");
    Mods::new(haest).with(m)
}

#[tokio::test]
async fn a_ts_mod_denies_rm_rf_root_in_bash_and_lets_everything_else_run() {
    let mods = with_mod(TestUi::new(), "guard.ts", GUARD_TS).await;
    let bash = Probe::new("Bash");
    let tools = wrapped(mods, Tools::from_iter([bash.clone()]));
    match tools[0]
        .execute(input(json!({"command": "rm -rf /"})), &ctx())
        .await
    {
        Err(ToolError::Input(m)) => assert!(m.contains("not on my watch")),
        other => panic!("not refused: {other:?}"),
    }
    tools[0]
        .execute(input(json!({"command": "ls"})), &ctx())
        .await
        .unwrap();
    assert_eq!(bash.ran().len(), 1);
}

#[tokio::test]
async fn a_mod_adds_a_tool_that_runs_in_its_runtime() {
    let source = r#"export default function (haest) {
      haest.tool({ name: 'Shout', description: 'louder', readOnly: true,
        parameters: { type: 'object', properties: { v: { type: 'string' } } },
        execute: async (input, ctx) => ({ content: String(input.v).toUpperCase() + '@' + ctx.cwd }) })
    }"#;
    let mods = with_mod(TestUi::new(), "shout.js", source).await;
    let tools = wrapped(mods, Tools::new());
    let shout = find(&tools, "Shout");
    assert!(shout.read_only());
    assert_eq!(shout.parameters()["properties"]["v"]["type"], "string");
    let out = shout
        .execute(input(json!({"v": "hi"})), &ctx())
        .await
        .unwrap();
    assert_eq!(out.content, format!("HI@{CWD}"));
}

#[tokio::test]
async fn prompt_submit_rewrites_the_humans_message_and_input_rewrites_reach_the_tool() {
    let source = r#"export default function (haest) {
      haest.on('prompt.submit', text => text + ' (be brief)')
      haest.on('tool.before', call => ({ input: { ...call.input, v: call.input.v + '!' } }))
      haest.on('tool.after', (call, out) => ({ ...out, content: out.content.toUpperCase() }))
    }"#;
    let mods = with_mod(TestUi::new(), "rewrite.js", source).await;
    assert_eq!(mods.rewrite_prompt("fix it").await, "fix it (be brief)");
    let tools = wrapped(mods, Tools::from_iter([Probe::new("Echo")]));
    let out = tools[0].execute(input(json!({"v": "hi"})), &ctx()).await;
    assert_eq!(out.unwrap().content, "ECHO:HI!");
}

#[tokio::test]
async fn commands_system_prompt_and_notices_from_a_mod_take_effect() {
    let source = r#"export default function (haest) {
      haest.command('/hello', args => 'say hello to ' + args)
      haest.command('quiet', () => { haest.notice('handled') })
      haest.on('system.prompt', () => 'House rule: ' + haest.workspace)
      haest.notice('loaded in ' + haest.cwd)
    }"#;
    let ui = TestUi::new();
    let mods = with_mod(ui.clone(), "house.js", source).await;
    assert_eq!(ui.infos(), [format!("loaded in {CWD}")]);
    assert_eq!(mods.commands(), ["hello", "quiet"]);
    assert_eq!(
        mods.command("/hello", "bob").await,
        CommandOutcome::Prompt("say hello to bob".into())
    );
    assert_eq!(mods.command("quiet", "").await, CommandOutcome::Handled);
    assert_eq!(ui.infos().len(), 2);
    assert_eq!(mods.system_append().await, "House rule: sandbox");
}

#[tokio::test]
async fn a_throwing_mod_is_reported_and_the_host_carries_on() {
    let source = r#"export default function (haest) {
      haest.on('tool.before', () => { throw new Error('boom') })
      haest.on('prompt.submit', () => { throw new Error('bad') })
    }"#;
    let ui = TestUi::new();
    let mods = with_mod(ui.clone(), "thrower.js", source).await;
    assert_eq!(mods.rewrite_prompt("as is").await, "as is");
    let bash = Probe::new("Bash");
    let tools = wrapped(mods, Tools::from_iter([bash.clone()]));
    let refused = tools[0]
        .execute(input(json!({"command": "ls"})), &ctx())
        .await;
    assert!(refused.is_err(), "a guard that broke lets nothing through");
    assert!(bash.ran().is_empty());
    let errors = ui.errors();
    assert_eq!(errors.len(), 2);
    assert!(errors.iter().all(|e| e.contains("thrower.js")));
}

#[tokio::test]
async fn a_mod_that_fails_to_load_says_why() {
    use ModLoadError as E;
    type Is = fn(&E) -> bool;
    let haest = env(TestUi::new(), WorkspaceMode::Sandbox);
    let cases: [(&str, &str, Is); 6] = [
        (
            "throws.js",
            "export default function () { throw new Error('nope') }",
            |e| matches!(e, E::Export(_)),
        ),
        ("syntax.ts", "export default function (haest: { {", |e| {
            matches!(e, E::TypeScript(_))
        }),
        ("syntax.js", "export default function (", |e| {
            matches!(e, E::Evaluate(_))
        }),
        ("value.js", "export default 42", |e| {
            matches!(e, E::NotAFunction)
        }),
        (
            "imports.js",
            "import x from './other.js'\nexport default function () {}",
            |e| matches!(e, E::Evaluate(_)),
        ),
        (
            "unknown.js",
            "export default function (haest) { haest.on('tool.during', () => {}) }",
            |e| matches!(e, E::Export(_)),
        ),
    ];
    for (file, source, why) in cases {
        match js(file, source, &haest).await {
            Ok(_) => panic!("{file} must not load"),
            Err(e) => assert!(why(&e) && !e.to_string().is_empty(), "{file}: {e:?}"),
        }
    }
}

#[tokio::test]
async fn a_default_export_that_never_returns_is_stopped_at_the_hosts_load_timeout() {
    let haest = env(TestUi::new(), WorkspaceMode::Sandbox);
    let wait = Duration::from_millis(200);
    let caps = ModCaps {
        load_timeout: wait,
        ..test_caps()
    };
    let hangs = "export default () => new Promise(() => {})";
    let loaded = JsMod::load_with(Path::new("hangs.js"), hangs.into(), &haest, caps).await;
    assert!(matches!(loaded, Err(ModLoadError::Timeout(t)) if t == wait));
}

#[tokio::test]
async fn a_mod_asks_the_human_and_their_answer_decides() {
    let source = r#"export default function (haest) {
      haest.on('tool.before', async (call, ctx) => {
        const verdict = await haest.ask({ toolName: call.name, description: 'run it?', inputPreview: String(call.input.command) }, ctx.signal)
        return verdict === 'allow' ? undefined : { deny: 'the human said no' }
      })
    }"#;
    let ui = TestUi::answering([Decision::Deny, Decision::Allow]);
    let mods = with_mod(ui.clone(), "asker.js", source).await;
    let bash = Probe::new("Bash");
    let tools = wrapped(mods, Tools::from_iter([bash.clone()]));
    let call = || input(json!({"command": "make"}));
    assert!(tools[0].execute(call(), &ctx()).await.is_err());
    tools[0].execute(call(), &ctx()).await.unwrap();
    assert_eq!(bash.ran().len(), 1);
    let asks = ui.asks();
    assert_eq!(asks.len(), 2);
    assert_eq!(
        (asks[0].tool_name.as_str(), asks[0].input_preview.as_str()),
        ("Bash", "make")
    );
}

#[tokio::test]
async fn an_ask_under_ctx_signal_ends_when_the_call_is_interrupted() {
    let source = r#"export default function (haest) {
      haest.on('tool.before', async (call, ctx) =>
        (await haest.ask({ toolName: 'Bash', description: 'd', inputPreview: 'p' }, ctx.signal)) === 'allow'
          ? undefined : { deny: 'no' })
    }"#;
    let ui = TestUi::holding();
    let mods = with_mod(ui.clone(), "held.js", source).await;
    let bash = Probe::new("Bash");
    let tools = wrapped(mods, Tools::from_iter([bash.clone()]));
    let c = ctx();
    let cancel = c.cancel().clone();
    let (out, ()) = tokio::join!(tools[0].execute(input(json!({})), &c), async {
        while ui.asks().is_empty() {
            tokio::task::yield_now().await;
        }
        cancel.cancel();
    });
    assert!(out.is_err());
    assert!(bash.ran().is_empty());
    assert!(ui.errors().is_empty(), "an interrupt is not a mod failure");
}

#[tokio::test]
async fn a_runaway_mod_is_stopped_and_its_next_call_still_works() {
    let source = r#"export default function (haest) {
      haest.on('tool.before', call => { if (call.input.v === 'loop') for (;;) {} })
    }"#;
    let ui = TestUi::new();
    let mods = with_mod(ui.clone(), "spin.js", source).await;
    let echo = Probe::new("Echo");
    let tools = wrapped(mods, Tools::from_iter([echo.clone()]));
    assert!(
        tools[0]
            .execute(input(json!({"v": "loop"})), &ctx())
            .await
            .is_err()
    );
    let out = tools[0].execute(input(json!({"v": "ok"})), &ctx()).await;
    assert_eq!(out.unwrap().content, "echo:ok");
    assert_eq!(ui.errors().len(), 1);
}
