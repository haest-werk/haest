//! Real child processes (`/bin/sh`), no network.

use std::{
    path::PathBuf,
    time::{SystemTime, UNIX_EPOCH},
};

use super::*;

/// A fresh directory under the system temp dir, removed on drop.
struct TempDir(PathBuf);

impl TempDir {
    fn new() -> Self {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("kern-mcp-{}-{nanos}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir.canonicalize().unwrap())
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn sh(name: &str, script: &str, dir: &TempDir, env: &[(&str, &str)]) -> McpServerConfig {
    McpServerConfig {
        name: name.into(),
        transport: McpTransport::Stdio {
            command: "/bin/sh".into(),
            args: vec!["-c".into(), script.into()],
            env: env
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            cwd: Some(dir.0.clone()),
        },
    }
}

/// The smallest MCP server sh can be: one tool, `pid`, answering with the server's process id.
const SH_SERVER: &str = r#"
while IFS= read -r line; do
  id=$(printf '%s' "$line" | sed -n 's/.*"id":\([0-9][0-9]*\).*/\1/p')
  case "$line" in
    *'"initialize"'*) printf '{"jsonrpc":"2.0","id":%s,"result":{"protocolVersion":"2025-06-18","capabilities":{"tools":{}},"serverInfo":{"name":"sh","version":"1"}}}\n' "$id" ;;
    *'"tools/list"'*) printf '{"jsonrpc":"2.0","id":%s,"result":{"tools":[{"name":"pid","inputSchema":{"type":"object"}}]}}\n' "$id" ;;
    *'"tools/call"'*) printf '{"jsonrpc":"2.0","id":%s,"result":{"content":[{"type":"text","text":"%s"}]}}\n' "$id" "$$" ;;
  esac
done
"#;

fn alive(pid: &str) -> bool {
    std::process::Command::new("kill")
        .args(["-0", pid])
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

#[tokio::test]
async fn a_stdio_server_answers_calls_and_its_process_ends_on_close() {
    let dir = TempDir::new();
    let hub = McpHub::new();
    hub.connect(&[sh("local", SH_SERVER, &dir, &[])], &|_| {})
        .await;
    assert_eq!(hub.failures(), []);
    let tools = hub.tools();
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0].name(), "mcp__local__pid");
    let out = tools[0].execute(JsonMap::new(), &ctx()).await.unwrap();
    let pid = out.content;
    assert!(alive(&pid), "the server is not running while connected");
    hub.close().await;
    assert!(!alive(&pid), "the server process outlived close");
}

#[tokio::test]
async fn a_stdio_server_runs_in_its_cwd_with_its_env_over_this_process_env() {
    let dir = TempDir::new();
    // Not an MCP server: it records what it was given and exits, so connecting fails.
    let script = r#"printf '%s|%s|%s' "$KERN_MCP_TEST" "$(pwd -P)" "${PATH:+inherited}" > seen"#;
    let hub = McpHub::new();
    hub.connect(
        &[sh("probe", script, &dir, &[("KERN_MCP_TEST", "v")])],
        &|_| {},
    )
    .await;
    assert_eq!(hub.failures()[0].name, "probe");
    let seen = std::fs::read_to_string(dir.0.join("seen")).unwrap();
    assert_eq!(seen, format!("v|{}|inherited", dir.0.display()));
}
