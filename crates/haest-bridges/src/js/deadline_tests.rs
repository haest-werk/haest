//! B1 (2026-11-24): a call into a loaded mod file whose promise nothing
//! resolves waited for its call's cancel token, and a command, which has
//! none, waited forever. Each call has `VmCaps::call_timeout` to settle now;
//! the human's time on a `haest.ask` does not count.

use std::{path::Path, sync::Arc, time::Duration};

use async_trait::async_trait;
use haest_core::{tool::input, workspace::WorkspaceMode};
use haest_mods::{ApprovalDecision, ApprovalRequest, Mod, ModToolCall, ModUi};
use serde_json::json;
use tokio_util::sync::CancellationToken;

use super::JsMod;
use crate::testkit::*;

const CALL: Duration = Duration::from_millis(300);

/// Far past `CALL`: a test that hangs fails here instead of hanging the suite.
const OUTER: Duration = Duration::from_secs(10);

async fn load(file: &str, source: &str, ui: Arc<dyn ModUi>) -> (JsMod, haest_mods::ModEnv) {
    let haest = haest_mods::ModEnv::new(CWD, WorkspaceMode::Sandbox, ui);
    let caps = test_caps().with_call_timeout(CALL);
    let m = JsMod::load_with(Path::new(file), source.into(), &haest, caps)
        .await
        .expect("the mod loads");
    (m, haest)
}

fn bash() -> ModToolCall {
    ModToolCall {
        name: "Bash".into(),
        input: input(json!({"command": "make"})).unwrap(),
        read_only: false,
    }
}

#[tokio::test]
async fn a_handler_whose_promise_never_settles_fails_at_the_call_timeout_and_the_mod_runs_on() {
    let source = "export default function (haest) {
      haest.on('tool.before', () => new Promise(() => {}))
      haest.on('prompt.submit', text => text + '!')
    }";
    let (m, haest) = load("forever.js", source, TestUi::new()).await;
    let got = tokio::time::timeout(OUTER, m.tool_before(&bash(), &ctx(), &haest))
        .await
        .expect("the call ended");
    let err = got
        .expect_err("a call that never settled is a failure")
        .to_string();
    assert!(
        err.contains("tool.before") && err.contains("did not settle"),
        "{err}"
    );
    // The mod runs on: the next call is answered.
    assert_eq!(
        m.prompt_submit("hi", &haest).await.unwrap().as_deref(),
        Some("hi!")
    );
}

#[tokio::test]
async fn a_command_whose_promise_never_settles_fails_at_the_call_timeout() {
    let source = "export default function (haest) {
      haest.command('hang', () => new Promise(() => {}))
    }";
    let (m, haest) = load("hang.js", source, TestUi::new()).await;
    let got = tokio::time::timeout(OUTER, m.command("hang", "", &haest))
        .await
        .expect("the command ended");
    let err = got
        .expect_err("a command that never settled is a failure")
        .to_string();
    assert!(err.contains("did not settle"), "{err}");
}

/// A human who takes longer than a call's timeout to answer.
#[derive(Debug)]
struct SlowHuman(Duration);

#[async_trait]
impl ModUi for SlowHuman {
    async fn ask(&self, _req: ApprovalRequest, _cancel: &CancellationToken) -> ApprovalDecision {
        tokio::time::sleep(self.0).await;
        ApprovalDecision::Allow
    }
    fn info(&self, _text: &str) {}
    fn error(&self, _text: &str) {}
}

#[tokio::test]
async fn the_humans_time_on_an_ask_does_not_count_against_the_call() {
    let source = r#"export default function (haest) {
      haest.on('tool.before', async (call, ctx) =>
        (await haest.ask({ toolName: call.name, description: 'run it?', inputPreview: 'make' }, ctx.signal)) === 'allow'
          ? { input: { command: 'make -j2' } } : { deny: 'no' })
    }"#;
    let (m, haest) = load("asks.js", source, Arc::new(SlowHuman(CALL * 3))).await;
    let got = tokio::time::timeout(OUTER, m.tool_before(&bash(), &ctx(), &haest))
        .await
        .expect("the call ended");
    let decision = got.expect("the human answered in time; the ask's wait is not the mod's");
    assert_eq!(
        decision,
        haest_mods::Before::Input(input(json!({"command": "make -j2"})).unwrap())
    );
}
