use std::sync::Arc;

use kern_core::{tool::ToolError, workspace::WorkspaceMode};
use serde_json::json;

use crate::{
    api::Decision,
    registry::{CommandOutcome, Mods},
    testkit::*,
};

const GUARD_TS: &str = r#"
interface Call { name: string; input: Record<string, unknown>; readOnly: boolean }
type Verdict = { deny: string } | undefined
const DANGER: RegExp = /rm -rf \//
export default function (kern: { on(event: string, fn: (call: Call) => Verdict): void }): void {
  kern.on('tool.before', (call: Call): Verdict =>
    call.name === 'Bash' && DANGER.test(String(call.input.command)) ? { deny: 'not on my watch' } : undefined)
}
"#;

/// `mods` with one file mod loaded from `source`.
async fn with_mod(host: Arc<TestHost>, file: &str, source: &str) -> Mods {
    let kern = env(host, WorkspaceMode::Sandbox);
    let m = js(file, source, &kern).await.expect("the mod loads");
    let mut mods = Mods::new(kern);
    mods.add(Arc::new(m));
    mods
}

#[tokio::test]
async fn a_ts_mod_denies_rm_rf_root_in_bash_and_lets_everything_else_run() {
    let mods = with_mod(TestHost::new(), "guard.ts", GUARD_TS).await;
    let bash = Probe::new("Bash");
    let tools = wrapped(mods, vec![bash.clone()]);
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
    let source = r#"export default function (kern) {
      kern.tool({ name: 'Shout', description: 'louder', readOnly: true,
        parameters: { type: 'object', properties: { v: { type: 'string' } } },
        execute: async (input, ctx) => ({ content: String(input.v).toUpperCase() + '@' + ctx.cwd }) })
    }"#;
    let mods = with_mod(TestHost::new(), "shout.js", source).await;
    let tools = wrapped(mods, Vec::new());
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
    let source = r#"export default function (kern) {
      kern.on('prompt.submit', text => text + ' (be brief)')
      kern.on('tool.before', call => ({ input: { ...call.input, v: call.input.v + '!' } }))
      kern.on('tool.after', (call, out) => ({ ...out, content: out.content.toUpperCase() }))
    }"#;
    let mods = with_mod(TestHost::new(), "rewrite.js", source).await;
    assert_eq!(mods.rewrite_prompt("fix it").await, "fix it (be brief)");
    let tools = wrapped(mods, vec![Probe::new("Echo")]);
    let out = tools[0].execute(input(json!({"v": "hi"})), &ctx()).await;
    assert_eq!(out.unwrap().content, "ECHO:HI!");
}

#[tokio::test]
async fn commands_system_prompt_and_notices_from_a_mod_take_effect() {
    let source = r#"export default function (kern) {
      kern.command('/hello', args => 'say hello to ' + args)
      kern.command('quiet', () => { kern.notice('handled') })
      kern.on('system.prompt', () => 'House rule: ' + kern.workspace)
      kern.notice('loaded in ' + kern.cwd)
    }"#;
    let host = TestHost::new();
    let mods = with_mod(host.clone(), "house.js", source).await;
    assert_eq!(host.notices(), [format!("loaded in {CWD}")]);
    assert_eq!(mods.commands(), ["hello", "quiet"]);
    assert_eq!(
        mods.command("/hello", "bob").await,
        CommandOutcome::Prompt("say hello to bob".into())
    );
    assert_eq!(mods.command("quiet", "").await, CommandOutcome::Handled);
    assert_eq!(host.notices().len(), 2);
    assert_eq!(mods.system_append().await, "House rule: sandbox");
}

#[tokio::test]
async fn a_throwing_mod_is_reported_and_the_host_carries_on() {
    let source = r#"export default function (kern) {
      kern.on('tool.before', () => { throw new Error('boom') })
      kern.on('prompt.submit', () => { throw new Error('bad') })
    }"#;
    let host = TestHost::new();
    let mods = with_mod(host.clone(), "thrower.js", source).await;
    assert_eq!(mods.rewrite_prompt("as is").await, "as is");
    let bash = Probe::new("Bash");
    let tools = wrapped(mods, vec![bash.clone()]);
    let refused = tools[0]
        .execute(input(json!({"command": "ls"})), &ctx())
        .await;
    assert!(refused.is_err(), "a guard that broke lets nothing through");
    assert!(bash.ran().is_empty());
    let errors = host.errors();
    assert_eq!(errors.len(), 2);
    assert!(errors.iter().all(|e| e.contains("thrower.js")));
}

#[tokio::test]
async fn a_mod_that_fails_to_load_says_why() {
    let kern = env(TestHost::new(), WorkspaceMode::Sandbox);
    for (file, source) in [
        (
            "throws.js",
            "export default function () { throw new Error('nope') }",
        ),
        ("syntax.ts", "export default function (kern: { {"),
        ("value.js", "export default 42"),
        (
            "imports.js",
            "import x from './other.js'\nexport default function () {}",
        ),
        (
            "unknown.js",
            "export default function (kern) { kern.on('tool.during', () => {}) }",
        ),
    ] {
        let err = js(file, source, &kern).await.err();
        assert!(err.is_some_and(|e| !e.is_empty()), "{file} must not load");
    }
}

#[tokio::test]
async fn a_mod_asks_the_human_and_their_answer_decides() {
    let source = r#"export default function (kern) {
      kern.on('tool.before', async (call, ctx) => {
        const verdict = await kern.ask({ toolName: call.name, description: 'run it?', inputPreview: String(call.input.command) }, ctx.signal)
        return verdict === 'allow' ? undefined : { deny: 'the human said no' }
      })
    }"#;
    let host = TestHost::answering([Decision::Deny, Decision::Allow]);
    let mods = with_mod(host.clone(), "asker.js", source).await;
    let bash = Probe::new("Bash");
    let tools = wrapped(mods, vec![bash.clone()]);
    let call = || input(json!({"command": "make"}));
    assert!(tools[0].execute(call(), &ctx()).await.is_err());
    tools[0].execute(call(), &ctx()).await.unwrap();
    assert_eq!(bash.ran().len(), 1);
    let asks = host.asks();
    assert_eq!(asks.len(), 2);
    assert_eq!(
        (asks[0].tool_name.as_str(), asks[0].input_preview.as_str()),
        ("Bash", "make")
    );
}

#[tokio::test]
async fn an_ask_under_ctx_signal_ends_when_the_call_is_interrupted() {
    let source = r#"export default function (kern) {
      kern.on('tool.before', async (call, ctx) =>
        (await kern.ask({ toolName: 'Bash', description: 'd', inputPreview: 'p' }, ctx.signal)) === 'allow'
          ? undefined : { deny: 'no' })
    }"#;
    let host = TestHost::holding();
    let mods = with_mod(host.clone(), "held.js", source).await;
    let bash = Probe::new("Bash");
    let tools = wrapped(mods, vec![bash.clone()]);
    let c = ctx();
    let cancel = c.cancel.clone();
    let (out, ()) = tokio::join!(tools[0].execute(input(json!({})), &c), async {
        while host.asks().is_empty() {
            tokio::task::yield_now().await;
        }
        cancel.cancel();
    });
    assert!(out.is_err());
    assert!(bash.ran().is_empty());
    assert!(
        host.errors().is_empty(),
        "an interrupt is not a mod failure"
    );
}

#[tokio::test]
async fn a_runaway_mod_is_stopped_and_its_next_call_still_works() {
    let source = r#"export default function (kern) {
      kern.on('tool.before', call => { if (call.input.v === 'loop') for (;;) {} })
    }"#;
    let host = TestHost::new();
    let mods = with_mod(host.clone(), "spin.js", source).await;
    let echo = Probe::new("Echo");
    let tools = wrapped(mods, vec![echo.clone()]);
    assert!(
        tools[0]
            .execute(input(json!({"v": "loop"})), &ctx())
            .await
            .is_err()
    );
    let out = tools[0].execute(input(json!({"v": "ok"})), &ctx()).await;
    assert_eq!(out.unwrap().content, "echo:ok");
    assert_eq!(host.errors().len(), 1);
}
