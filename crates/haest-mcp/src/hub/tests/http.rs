//! An HTTP server whose URL carries its key in the query (`?api_key=`): the
//! key reaches no failure, log line, Debug output or tool answer, whichever
//! request breaks. The server is a few lines of streamable HTTP on a local
//! port (JSON answers, no SSE stream), no network beyond it.

use std::{
    io::{Read, Write},
    net::{TcpListener, TcpStream},
};

use super::*;

const KEY: &str = "tok-SECRET-123";

/// Serve MCP at `/mcp?api_key=<KEY>` on a local port, dropping the
/// connection without an answer on a request for `drop_on` (`tools/list`,
/// `tools/call`). Returns the URL, key included.
fn serve(drop_on: &'static str) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!(
        "http://{}/mcp?api_key={KEY}",
        listener.local_addr().unwrap()
    );
    std::thread::spawn(move || {
        for conn in listener.incoming().flatten() {
            std::thread::spawn(move || answer(conn, drop_on));
        }
    });
    url
}

/// One request, one answer, then the connection closes.
fn answer(mut conn: TcpStream, drop_on: &str) {
    let (head, body) = read_request(&mut conn);
    if !head.starts_with("POST") {
        // No SSE stream, no session to delete.
        let _ = conn.write_all(
            b"HTTP/1.1 405 Method Not Allowed\r\ncontent-length: 0\r\nconnection: close\r\n\r\n",
        );
        return;
    }
    let msg: Value = serde_json::from_slice(&body).unwrap_or_default();
    let method = msg["method"].as_str().unwrap_or_default();
    if method == drop_on {
        return;
    }
    let Some(id) = msg.get("id") else {
        let _ = conn
            .write_all(b"HTTP/1.1 202 Accepted\r\ncontent-length: 0\r\nconnection: close\r\n\r\n");
        return;
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
    let head = format!(
        "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\nmcp-session-id: s1\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
        reply.len()
    );
    let _ = conn.write_all(head.as_bytes());
    let _ = conn.write_all(reply.as_bytes());
}

/// A request's head and body.
fn read_request(conn: &mut impl Read) -> (String, Vec<u8>) {
    let mut buf = Vec::new();
    let mut byte = [0u8; 1];
    while !buf.ends_with(b"\r\n\r\n") && conn.read(&mut byte).unwrap_or(0) == 1 {
        buf.push(byte[0]);
    }
    let head = String::from_utf8_lossy(&buf).into_owned();
    let len = head
        .to_lowercase()
        .lines()
        .find_map(|l| {
            l.strip_prefix("content-length:")
                .map(str::trim)
                .map(str::to_string)
        })
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(0);
    let mut body = vec![0; len];
    let _ = conn.read_exact(&mut body);
    (head, body)
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
    let misread_host = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = misread_host.local_addr().unwrap().port();
    let (heard, requests) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        for mut conn in misread_host.incoming().flatten() {
            let _ = heard.send(read_request(&mut conn).0);
        }
    });
    let (hub, logs) = connect(format!("http://127.0.0.1:{port}/s3cret@mcp.example/mcp")).await;
    let sent = requests.try_recv().ok();
    assert_eq!(sent, None, "the misread host was sent a request");
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
    let (hub, logs) = connect(serve("tools/list")).await;
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
    let (hub, _logs) = connect(serve("tools/call")).await;
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
