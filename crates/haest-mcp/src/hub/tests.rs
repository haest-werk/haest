use std::time::Duration;

use haest_core::{
    ToolRef,
    messages::MediaKind,
    tool::{JsonMap, ToolContext, ToolError},
    workspace::ShutWorkspace,
};
use serde_json::{Value, json};
use tokio::task::JoinHandle;

use super::*;
use crate::{
    McpTransport,
    test_server::{TestServer, spawn},
};

/// A real wait that a paused clock skips at once.
const SETTLE: Duration = Duration::from_secs(5);

async fn connected(server: TestServer) -> (McpHub, JoinHandle<()>) {
    let (io, handle) = spawn(server);
    let hub = McpHub::new();
    hub.connect_transport("srv", io).await;
    assert!(hub.failures().is_empty(), "{:?}", hub.failures());
    (hub, handle)
}

/// A log that keeps its lines, and the lines it kept.
fn kept_log() -> (
    Arc<Mutex<Vec<String>>>,
    impl Fn(&str) + Send + Sync + 'static,
) {
    let logs = Arc::new(Mutex::new(Vec::new()));
    let sink = logs.clone();
    (logs, move |m: &str| {
        sink.lock().unwrap().push(m.to_string())
    })
}

fn tool(hub: &McpHub, name: &str) -> ToolRef {
    let full = format!("mcp__srv__{name}");
    hub.tools().into_iter().find(|t| t.name() == full).unwrap()
}

fn ctx() -> ToolContext {
    ToolContext::new("/", Arc::new(ShutWorkspace))
}

fn input(v: Value) -> JsonMap {
    v.as_object().unwrap().clone()
}

#[tokio::test]
async fn a_server_that_cannot_start_is_recorded_as_a_failure_not_thrown() {
    let (logs, log) = kept_log();
    let hub = McpHub::new().with_log(log);
    hub.connect(&[McpServerConfig::new(
        "broken",
        McpTransport::stdio("/nonexistent/binary"),
    )])
    .await;
    assert!(hub.tools().is_empty());
    let names: Vec<_> = hub
        .failures()
        .iter()
        .map(|f| f.name().to_string())
        .collect();
    assert_eq!(names, ["broken"]);
    assert_eq!(logs.lock().unwrap().len(), 1);
    hub.close().await;
}

#[tokio::test]
async fn a_connected_server_offers_its_tools_with_canonical_names_kinds_and_hints() {
    let (logs, log) = kept_log();
    let (io, _server) = spawn(TestServer::default());
    let hub = McpHub::new().with_log(log);
    hub.connect_transport("srv", io).await;
    let tools = hub.tools();
    let names: Vec<_> = tools.iter().map(|t| t.name()).collect();
    let all = ["echo", "shot", "fail", "reject", "hang"];
    assert_eq!(names, all.map(|t| format!("mcp__srv__{t}")));
    let kinds: Vec<_> = tools.iter().map(|t| t.kind()).collect();
    assert_eq!(kinds, all.map(|t| format!("mcp.srv.{t}")));
    // Only the server's own readOnlyHint makes a tool parallel-safe.
    let read_only: Vec<_> = tools
        .iter()
        .filter(|t| t.read_only())
        .map(|t| t.name())
        .collect();
    assert_eq!(read_only, ["mcp__srv__echo"]);
    assert_eq!(
        tool(&hub, "echo").parameters()["properties"]["text"]["type"],
        "string"
    );
    // A tool its server did not describe still says what it is and where it lives.
    let shot = tool(&hub, "shot").description();
    assert!(shot.contains("shot") && shot.contains("srv"));
    assert_eq!(logs.lock().unwrap().len(), 1);
    // The hub is a ToolSource: the loop sees the same tools.
    assert_eq!(ToolSource::tools(&hub).len(), all.len());
}

#[tokio::test]
async fn a_text_tool_answers_with_its_text() {
    let (hub, _server) = connected(TestServer::default()).await;
    let out = tool(&hub, "echo")
        .execute(input(json!({"text": "hi"})), &ctx())
        .await
        .unwrap();
    assert_eq!(out.content, "hi");
    assert!(!out.is_error && out.media.is_empty());
}

#[tokio::test]
async fn an_image_result_reaches_the_model_as_media() {
    let (hub, _server) = connected(TestServer::default()).await;
    let out = tool(&hub, "shot")
        .execute(JsonMap::new(), &ctx())
        .await
        .unwrap();
    assert_eq!(out.media.len(), 1);
    assert_eq!(out.media[0].kind, MediaKind::Image);
    assert_eq!(out.media[0].data, "AQID");
    assert_eq!(out.media[0].name, "image-2.png");
    assert_eq!(out.content.lines().count(), 2);
}

#[tokio::test]
async fn an_error_result_is_an_error_output_the_model_reads() {
    let (hub, _server) = connected(TestServer::default()).await;
    let out = tool(&hub, "fail")
        .execute(JsonMap::new(), &ctx())
        .await
        .unwrap();
    assert!(out.is_error);
    assert_eq!(out.content, "boom");
}

#[tokio::test]
async fn a_protocol_error_is_a_failed_call() {
    let (hub, _server) = connected(TestServer::default()).await;
    let res = tool(&hub, "reject").execute(JsonMap::new(), &ctx()).await;
    assert!(matches!(res, Err(ToolError::Failed(_))));
}

#[tokio::test]
async fn a_cancelled_call_returns_at_once_and_tells_the_server() {
    let server = TestServer::default();
    let (hub, _server) = connected(server.clone()).await;
    let hang = tool(&hub, "hang");
    let ctx = ctx();
    let cancel = ctx.cancel().clone();
    let call = tokio::spawn(async move { hang.execute(JsonMap::new(), &ctx).await });
    tokio::time::timeout(SETTLE, server.hanging.notified())
        .await
        .expect("the call never reached the server");
    cancel.cancel();
    let res = tokio::time::timeout(SETTLE, call).await.unwrap().unwrap();
    assert!(matches!(res, Err(ToolError::Failed(_))));
    tokio::time::timeout(SETTLE, server.cancelled.notified())
        .await
        .expect("the server never heard the call was cancelled");
}

#[tokio::test(start_paused = true)]
async fn a_call_that_never_answers_times_out_and_is_cancelled_on_the_server() {
    let server = TestServer::default();
    let (hub, _server) = connected(server.clone()).await;
    let started = tokio::time::Instant::now();
    let res = tool(&hub, "hang").execute(JsonMap::new(), &ctx()).await;
    assert!(matches!(res, Err(ToolError::Failed(_))));
    assert!(started.elapsed() >= CALL_TIMEOUT);
    tokio::time::timeout(SETTLE, server.cancelled.notified())
        .await
        .expect("the server never heard the call was abandoned");
}

#[tokio::test(start_paused = true)]
async fn a_server_that_never_answers_is_a_failure_after_the_connect_timeout() {
    let (io, _silent) = tokio::io::duplex(64 * 1024);
    let hub = McpHub::new();
    let started = tokio::time::Instant::now();
    hub.connect_transport("mute", io).await;
    assert!(started.elapsed() >= CONNECT_TIMEOUT);
    assert_eq!(hub.failures()[0].name(), "mute");
    assert!(hub.tools().is_empty());
}

#[tokio::test]
async fn close_shuts_every_server_down_and_its_tools_fail_after() {
    let (hub, server) = connected(TestServer::default()).await;
    let echo = tool(&hub, "echo");
    hub.close().await;
    tokio::time::timeout(SETTLE, server)
        .await
        .expect("the server is still connected after close")
        .unwrap();
    assert!(hub.tools().is_empty());
    let res = echo.execute(input(json!({"text": "x"})), &ctx()).await;
    assert!(res.is_err());
}

#[tokio::test]
async fn dropping_the_hub_shuts_the_servers_down() {
    let (hub, server) = connected(TestServer::default()).await;
    drop(hub);
    tokio::time::timeout(SETTLE, server)
        .await
        .expect("the server is still connected after the hub was dropped")
        .unwrap();
}

#[tokio::test]
async fn dropping_one_clone_keeps_the_servers_for_the_others() {
    let (hub, mut server) = connected(TestServer::default()).await;
    let kept = hub.clone();
    drop(hub);
    // Still connected: the server task has not ended, and a call still answers.
    assert!(
        tokio::time::timeout(Duration::from_millis(200), &mut server)
            .await
            .is_err(),
        "the server was shut down while a clone still held it"
    );
    let out = tool(&kept, "echo")
        .execute(input(json!({"text": "still here"})), &ctx())
        .await
        .unwrap();
    assert_eq!(out.content, "still here");
    drop(kept);
    tokio::time::timeout(SETTLE, server)
        .await
        .expect("the server is still connected after the last clone was dropped")
        .unwrap();
}

#[test]
fn a_host_can_spawn_connect_and_close() {
    fn spawnable<F: std::future::Future + Send>(_: F) {}
    let hub = McpHub::new();
    spawnable(hub.connect(&[]));
    spawnable(hub.close());
}

mod http;
mod names;
#[cfg(unix)]
mod stdio;
