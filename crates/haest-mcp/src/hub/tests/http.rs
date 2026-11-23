//! An HTTP server whose URL carries its key in the query (`?api_key=`): the
//! key reaches no failure, log line, Debug output or tool answer, whichever
//! request breaks. The server is a few lines of streamable HTTP on a local
//! port (JSON answers, no SSE stream), no network beyond it.

use std::net::TcpListener;

use haest_http::testing::{Respond, Seen, Server};

use super::*;

const KEY: &str = "tok-SECRET-123";

/// MCP on a local port, closing the connection without an answer on a
/// request for `drop_on` (`tools/list`, `tools/call`).
fn serve(drop_on: &'static str) -> Server {
    Server::answering(move |seen| answer(seen, drop_on))
}

/// Where `server` serves MCP: `/mcp?api_key=<KEY>`, key included.
fn keyed_url(server: &Server) -> String {
    server.url(&format!("/mcp?api_key={KEY}"))
}

/// One request's answer, as an MCP server with no SSE stream gives it.
fn answer(seen: &Seen, drop_on: &str) -> Respond {
    if !seen.line.starts_with("POST ") {
        // No SSE stream, no session to delete.
        return Respond::WithHeaders(405, Vec::new(), String::new());
    }
    let msg: Value = serde_json::from_slice(&seen.body).unwrap_or_default();
    let method = msg["method"].as_str().unwrap_or_default();
    if method == drop_on {
        return Respond::Close;
    }
    let Some(id) = msg.get("id") else {
        return Respond::WithHeaders(202, Vec::new(), String::new());
    };
    let result = match method {
        "initialize" => json!({
            "protocolVersion": msg["params"]["protocolVersion"],
            "capabilities": {"tools": {}},
            "serverInfo": {"name": "keyed", "version": "1"},
        }),
        "tools/list" => json!({"tools": [{"name": "echo", "inputSchema": {"type": "object"}}]}),
        _ => json!({"content": [{"type": "text", "text": "ok"}]}),
    };
    let reply = json!({"jsonrpc": "2.0", "id": id, "result": result}).to_string();
    let headers = vec![
        ("content-type".into(), "application/json".into()),
        ("mcp-session-id".into(), "s1".into()),
    ];
    Respond::WithHeaders(200, headers, reply)
}

/// Connect `url` as the server `keyed`, keeping the log.
async fn connect(url: String) -> (McpHub, Arc<Mutex<Vec<String>>>) {
    let (logs, log) = kept_log();
    let hub = McpHub::new().with_log(log);
    hub.connect(&[McpServerConfig::new("keyed", McpTransport::http(url))])
        .await;
    (hub, logs)
}

/// Everything a host or the model could be shown of the hub: the failures
/// as `report` gives them (what haest-runtime warns with), the log lines,
/// and the Debug output.
fn everything_shown(hub: &McpHub, logs: &Mutex<Vec<String>>) -> String {
    let failures = hub.failures();
    let reports = failures.iter().map(|f| report(f.error()));
    let mut shown: Vec<String> = reports.collect();
    shown.extend(logs.lock().unwrap().iter().cloned());
    shown.push(format!("{hub:?}"));
    shown.push(format!("{failures:?}"));
    shown.join("\n")
}

#[tokio::test]
async fn a_server_that_cannot_be_reached_fails_without_showing_its_urls_key() {
    let closed = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/mcp?api_key={KEY}", closed.local_addr().unwrap());
    drop(closed);
    let (hub, logs) = connect(url).await;
    let failures = hub.failures();
    assert!(matches!(failures[0].error(), McpStartError::Initialize(_)));
    let shown = everything_shown(&hub, &logs);
    assert!(!shown.contains(KEY), "the key in: {shown}");
    assert!(shown.contains("/mcp"), "where it went is gone: {shown}");
}

/// A password with an unescaped `/` reads to a URL parser as a host, a port
/// and a path: `http://127.0.0.1:<port>/s3cret@mcp.example/mcp` meant the
/// user `127.0.0.1` with the password `<port>/s3cret`, and a parser sends
/// 127.0.0.1 on that port the rest of the password in the request line,
/// the server's headers with it. The server fails before anything is sent.
#[tokio::test]
async fn a_url_a_parser_misreads_fails_its_server_before_anything_is_sent() {
    let misread_host = Server::start([]);
    let port = misread_host.addr().port();
    let (hub, logs) = connect(format!("http://127.0.0.1:{port}/s3cret@mcp.example/mcp")).await;
    let sent = misread_host.seen();
    assert!(sent.is_empty(), "the misread host was sent {sent:?}");
    let failures = hub.failures();
    assert!(
        matches!(
            failures.first().map(McpFailure::error),
            Some(McpStartError::Url(_))
        ),
        "{failures:?}"
    );
    let shown = everything_shown(&hub, &logs);
    assert!(!shown.contains("s3cret"), "{shown}");
    assert!(
        shown.contains("%2F") && shown.contains("mcp.example/mcp"),
        "{shown}"
    );
}

#[tokio::test]
async fn a_server_that_breaks_off_listing_its_tools_fails_without_showing_its_urls_key() {
    let server = serve("tools/list");
    let (hub, logs) = connect(keyed_url(&server)).await;
    let failures = hub.failures();
    assert!(
        matches!(failures[0].error(), McpStartError::ListTools(_)),
        "{failures:?}"
    );
    let shown = everything_shown(&hub, &logs);
    assert!(!shown.contains(KEY), "the key in: {shown}");
}

#[tokio::test]
async fn a_call_that_breaks_off_tells_the_model_without_the_urls_key() {
    let server = serve("tools/call");
    let (hub, _logs) = connect(keyed_url(&server)).await;
    assert!(hub.failures().is_empty(), "{:?}", hub.failures());
    let echo = hub.tools().into_iter().next().unwrap();
    let err = echo.execute(JsonMap::new(), &ctx()).await.unwrap_err();
    let ToolError::Failed(why) = &err else {
        panic!("{err:?}");
    };
    assert!(!why.contains(KEY), "the key in: {why}");
    assert!(!format!("{err:?}").contains(KEY), "the key in: {err:?}");
    hub.close().await;
}
