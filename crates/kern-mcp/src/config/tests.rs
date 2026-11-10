use super::*;
use kern_core::fs::MemFs;
use serde_json::json;

const CWD: &str = "/p";

fn fs_with(config: Value) -> MemFs {
    MemFs::with_files([("/p/.mcp.json", config.to_string())])
}

fn no_vars(_: &str) -> Option<String> {
    None
}

async fn load(fs: &MemFs) -> Result<ProjectMcp, McpConfigError> {
    load_project_mcp_config(fs, Path::new(CWD), &no_vars).await
}

#[tokio::test]
async fn reads_stdio_and_http_servers_in_claude_code_format() {
    let fs = fs_with(json!({
        "mcpServers": {
            "local": {"command": "node", "args": ["s.js"], "env": {"A": "1"}},
            "remote": {"type": "http", "url": "https://x/mcp", "headers": {"H": "v"}},
            "junk": {"nothing": true},
        }
    }));
    let cfg = load(&fs).await.unwrap().servers;
    let kinds: Vec<_> = cfg
        .iter()
        .map(|c| format!("{}:{}", c.name, c.type_name()))
        .collect();
    assert_eq!(kinds, ["local:stdio", "remote:http"]);
    assert_eq!(
        cfg[0].transport,
        McpTransport::Stdio {
            command: "node".into(),
            args: vec!["s.js".into()],
            env: BTreeMap::from([("A".into(), "1".into())]),
            cwd: Some(PathBuf::from(CWD)),
        }
    );
    assert_eq!(
        cfg[1].transport,
        McpTransport::Http {
            url: "https://x/mcp".into(),
            headers: BTreeMap::from([("H".into(), "v".into())]),
        }
    );
}

/// Claude Code's expansion: `${VAR}` and `${VAR:-default}` in command, args,
/// env, url and headers values. An unset variable with no default is passed
/// as written, and the host is told which server names it.
#[tokio::test]
async fn variables_are_filled_in_where_claude_code_fills_them() {
    let fs = fs_with(json!({
        "mcpServers": {
            "local": {
                "command": "${BIN:-node}",
                "args": ["--root", "${ROOT}"],
                "env": {"TOKEN": "${TOKEN}", "LEFT": "${NOPE}"},
            },
            "remote": {
                "url": "${BASE:-https://api.example.com}/mcp",
                "headers": {"Authorization": "Bearer ${TOKEN}"},
            },
        }
    }));
    let vars = |name: &str| match name {
        "ROOT" => Some("/srv".to_string()),
        "TOKEN" => Some("t0k".to_string()),
        _ => None,
    };
    let got = load_project_mcp_config(&fs, Path::new(CWD), &vars)
        .await
        .unwrap();
    assert_eq!(
        got.servers[0].transport,
        McpTransport::Stdio {
            command: "node".into(),
            args: vec!["--root".into(), "/srv".into()],
            env: BTreeMap::from([
                ("LEFT".into(), "${NOPE}".into()),
                ("TOKEN".into(), "t0k".into()),
            ]),
            cwd: Some(PathBuf::from(CWD)),
        }
    );
    assert_eq!(
        got.servers[1].transport,
        McpTransport::Http {
            url: "https://api.example.com/mcp".into(),
            headers: BTreeMap::from([("Authorization".into(), "Bearer t0k".into())]),
        }
    );
    assert_eq!(got.notes.len(), 1, "{:?}", got.notes);
    assert!(got.notes[0].contains("local") && got.notes[0].contains("NOPE"));
}

#[tokio::test]
async fn no_file_means_no_servers() {
    assert!(load(&MemFs::new()).await.unwrap().servers.is_empty());
}

#[tokio::test]
async fn a_file_without_servers_means_no_servers() {
    let fs = fs_with(json!({"other": 1}));
    assert!(load(&fs).await.unwrap().servers.is_empty());
}

#[tokio::test]
async fn broken_json_is_an_error_the_host_can_report() {
    let fs = MemFs::with_files([("/p/.mcp.json", "{not json")]);
    assert!(matches!(load(&fs).await, Err(McpConfigError::Parse { .. })));
}

#[tokio::test]
async fn an_argv_that_is_not_all_strings_is_refused_naming_the_server() {
    let fs = fs_with(json!({"mcpServers": {"s": {"command": "x", "args": ["--port", 1]}}}));
    let err = load(&fs).await;
    assert!(matches!(err, Err(McpConfigError::Invalid { server, .. }) if server == "s"));
}
