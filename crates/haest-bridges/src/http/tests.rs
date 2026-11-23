//! What only a POST does: the request a server gets (its headers), a
//! status that is not 2xx, an answer that is no decision, and a server
//! that never answers, given up on when its time is up or the human
//! interrupts.

use std::{sync::Arc, time::Duration};

use haest_core::{
    ToolContext, ToolError, ToolOutput, Tools,
    tool::input,
    workspace::{ShutWorkspace, WorkspaceMode},
};
use haest_mods::{Binding, Hooks, Mods, OnError, ToolBeforePayload};
use serde_json::json;
use tokio_util::sync::CancellationToken;

use crate::{HttpHook, hook_tests::*};

/// A probe tool behind `hooks`, called once.
async fn call(
    hooks: Hooks,
    ui: Arc<TestUi>,
    ctx: &ToolContext,
) -> (Result<ToolOutput, ToolError>, usize) {
    let probe = Probe::new("Echo");
    let mods = Mods::new(env(ui, WorkspaceMode::Sandbox), [hooks]);
    let tools = wrapped(mods, Tools::from_iter([probe.clone()]));
    let out = tools[0]
        .execute(input(json!({ "v": "hi" })).unwrap(), ctx)
        .await;
    (out, probe.ran().len())
}

#[tokio::test]
async fn each_event_is_a_post_of_json_with_the_headers_set() {
    let server = Server::start([Respond::With(200, r#"{"deny":"no"}"#.into())]);
    let hook = HttpHook::new(server.url("/hook"))
        .with_header("Authorization", "Bearer old")
        .with_header("authorization", "Bearer t0k3n")
        .with_header("x-team", "core");
    let (refused, ran) = call(Hooks::new().on_tool_before(hook), TestUi::new(), &ctx()).await;
    assert!(refused.unwrap_err().to_string().contains("Refused: no"));
    assert_eq!(ran, 0);

    let seen = server.seen();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].line, "POST /hook HTTP/1.1");
    assert_eq!(seen[0].header("content-type"), Some("application/json"));
    // A header set twice, in any case: the last one, sent once.
    assert_eq!(seen[0].header("authorization"), Some("Bearer t0k3n"));
    assert_eq!(seen[0].header("x-team"), Some("core"));
}

#[tokio::test]
async fn a_status_that_is_not_2xx_or_an_answer_that_is_no_decision_follows_on_error() {
    for (respond, says) in [
        (
            Respond::With(503, "the reviewer is down".into()),
            "the server answered 503: the reviewer is down",
        ),
        (
            Respond::With(200, "ok!".into()),
            "the hook's answer is not a decision",
        ),
    ] {
        // tool.before's default is block: the call is refused.
        let server = Server::start([respond.clone()]);
        let ui = TestUi::new();
        let hook = HttpHook::new(server.url("/hook"));
        let (refused, ran) = call(
            Hooks::new().on_tool_before(hook.clone()),
            ui.clone(),
            &ctx(),
        )
        .await;
        let refused = refused.unwrap_err().to_string();
        assert!(refused.contains(says), "{refused}");
        assert_eq!(ran, 0);

        // `continue`: the human sees it, the call runs.
        let server = Server::start([respond]);
        let ui = TestUi::new();
        let hooks = Hooks::new().on_tool_before(
            Binding::<ToolBeforePayload>::new(HttpHook::new(server.url("/hook")))
                .with_on_error(OnError::Continue),
        );
        let (ok, ran) = call(hooks, ui.clone(), &ctx()).await;
        assert!(ok.is_ok());
        assert_eq!(ran, 1);
        assert!(
            ui.errors().iter().any(|e| e.contains(says)),
            "{:?}",
            ui.errors()
        );
    }
}

#[tokio::test]
async fn a_server_that_never_answers_is_given_up_on_at_the_timeout_and_on_an_interrupt() {
    let server = Server::start([Respond::Never]);
    let hooks = Hooks::new().on_tool_before(
        Binding::<ToolBeforePayload>::new(HttpHook::new(server.url("/hook")))
            .with_timeout(Duration::from_millis(300)),
    );
    let (refused, ran) = call(hooks, TestUi::new(), &ctx()).await;
    assert!(refused.unwrap_err().to_string().contains("did not answer"));
    assert_eq!(ran, 0);

    let server = Server::start([Respond::Never]);
    let cancel = CancellationToken::new();
    let ctx = ToolContext::new(CWD, ShutWorkspace).with_cancel(cancel.clone());
    let url = server.url("/hook");
    let seen = tokio::spawn(async move {
        eventually("the request arrived", || !server.seen().is_empty()).await;
        cancel.cancel();
        server
    });
    let (out, ran) = call(
        Hooks::new().on_tool_before(HttpHook::new(url)),
        TestUi::new(),
        &ctx,
    )
    .await;
    assert!(out.unwrap_err().to_string().contains("Interrupted"));
    assert_eq!(ran, 0);
    drop(seen.await.unwrap());
}

#[test]
fn its_debug_shows_no_credentials_and_no_header_values() {
    let hook = HttpHook::new("https://user:secret@review.internal/hook?token=abc")
        .with_header("authorization", "Bearer t0k3n");
    let shown = format!("{hook:?}");
    assert!(
        !shown.contains("secret") && !shown.contains("t0k3n"),
        "{shown}"
    );
    assert!(shown.contains("authorization"), "{shown}");
}
