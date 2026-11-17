//! Real child processes (`/bin/sh`), no network.

use std::{
    path::PathBuf,
    time::{SystemTime, UNIX_EPOCH},
};

use haest_core::ChildEnv;

use super::*;

/// A fresh directory under the system temp dir, removed on drop.
struct TempDir(PathBuf);

impl TempDir {
    fn new() -> Self {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("haest-mcp-{}-{nanos}", std::process::id()));
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
    let stdio = env.iter().fold(
        McpTransport::stdio("/bin/sh")
            .with_args(["-c", script])
            .with_cwd(&dir.0),
        |s, (k, v)| s.with_env_var(*k, *v),
    );
    McpServerConfig::new(name, stdio)
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
    // The server script runs sed: it needs a PATH, handed in like a host would.
    let hub = McpHub::new().with_env(std::env::vars().collect());
    hub.connect(&[sh("local", SH_SERVER, &dir, &[])]).await;
    assert!(hub.failures().is_empty(), "{:?}", hub.failures());
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
async fn a_stdio_server_process_ends_when_the_hub_is_dropped_though_its_tools_are_held() {
    let dir = TempDir::new();
    let hub = McpHub::new().with_env(std::env::vars().collect());
    hub.connect(&[sh("local", SH_SERVER, &dir, &[])]).await;
    // A loop still holds the tools it was handed when the host drops the hub.
    let tools = hub.tools();
    let pid = tools[0]
        .execute(JsonMap::new(), &ctx())
        .await
        .unwrap()
        .content;
    assert!(alive(&pid), "the server is not running while connected");
    drop(hub);
    // Dropping does not wait: the kill lands on the runtime shortly after.
    let gone = async {
        while alive(&pid) {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    };
    assert!(
        tokio::time::timeout(SETTLE, gone).await.is_ok(),
        "the server process outlived the hub"
    );
    assert!(tools[0].execute(JsonMap::new(), &ctx()).await.is_err());
}

/// `McpHub::new` hands a stdio server no PATH: a server named by a bare
/// command that is not in the system's default directories must fail saying
/// so, never as a bare "not found".
#[tokio::test]
async fn a_bare_command_with_no_path_handed_fails_naming_the_missing_path() {
    let bare = || {
        [McpServerConfig::new(
            "bare",
            McpTransport::stdio("haest-no-such-mcp-server"),
        )]
    };
    let hub = McpHub::new();
    hub.connect(&bare()).await;
    let failures = hub.failures();
    let error = failures[0].error();
    assert!(
        matches!(error, McpStartError::NotOnPath { .. }),
        "the missing PATH goes unnamed: {error:?}"
    );
    // With a PATH handed in, a missing program is only missing.
    let path = ChildEnv::new([("PATH", "/usr/bin:/bin")]);
    let hub = McpHub::new().with_env(path);
    hub.connect(&bare()).await;
    let failures = hub.failures();
    let error = failures[0].error();
    assert!(matches!(error, McpStartError::Spawn { .. }), "{error:?}");
}

#[tokio::test]
async fn a_stdio_server_runs_in_its_cwd_with_the_handed_env_and_its_own_and_nothing_else() {
    let dir = TempDir::new();
    // Not an MCP server: it records what it was given and exits, so connecting fails.
    let script =
        r#"printf '%s|%s|%s|%s' "$HAEST_MCP_TEST" "$(pwd -P)" "$HANDED" "${HOME-unset}" > seen"#;
    let hub = McpHub::new().with_env(ChildEnv::new([("HANDED", "h")]));
    hub.connect(&[sh("probe", script, &dir, &[("HAEST_MCP_TEST", "v")])])
        .await;
    assert_eq!(hub.failures()[0].name(), "probe");
    let seen = std::fs::read_to_string(dir.0.join("seen")).unwrap();
    assert_eq!(seen, format!("v|{}|h|unset", dir.0.display()));
}
