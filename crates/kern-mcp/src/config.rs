//! Which MCP servers to connect, and the project's `.mcp.json` (Claude Code format):
//! `{ "mcpServers": { "<name>": {command, args, env} | {type: "http", url, headers} } }`.

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

use kern_core::fs::{Fs, read_text_if_present};
use serde_json::{Map, Value};

/// A project's MCP servers: `<cwd>/.mcp.json`.
pub fn project_mcp_path(cwd: &Path) -> PathBuf {
    cwd.join(".mcp.json")
}

/// One MCP server to connect.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpServerConfig {
    /// The `<server>` in `mcp__<server>__<tool>`.
    pub name: String,
    pub transport: McpTransport,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum McpTransport {
    /// Streamable HTTP; `headers` ride on every request (an `Authorization` included).
    Http {
        url: String,
        headers: BTreeMap<String, String>,
    },
    /// A child process speaking MCP on stdin/stdout. It inherits this process's
    /// environment with `env` laid over it; its stderr is ignored.
    Stdio {
        command: String,
        args: Vec<String>,
        env: BTreeMap<String, String>,
        cwd: Option<PathBuf>,
    },
}

impl McpServerConfig {
    pub fn http(name: impl Into<String>, url: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            transport: McpTransport::Http {
                url: url.into(),
                headers: BTreeMap::new(),
            },
        }
    }

    pub fn stdio(name: impl Into<String>, command: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            transport: McpTransport::Stdio {
                command: command.into(),
                args: Vec::new(),
                env: BTreeMap::new(),
                cwd: None,
            },
        }
    }

    /// `"http"` or `"stdio"`, the `.mcp.json` type names.
    pub fn type_name(&self) -> &'static str {
        match self.transport {
            McpTransport::Http { .. } => "http",
            McpTransport::Stdio { .. } => "stdio",
        }
    }
}

/// Why `.mcp.json` could not be used. A host reports it and goes on without the file.
#[derive(Debug, thiserror::Error)]
pub enum McpConfigError {
    #[error("{}: {source}", path.display())]
    Read {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("{}: {source}", path.display())]
    Parse {
        path: PathBuf,
        source: serde_json::Error,
    },
    /// A field of a known server has the wrong type: refusing beats starting a
    /// program with an argv nobody wrote.
    #[error("{}: server \"{server}\": {problem}", path.display())]
    Invalid {
        path: PathBuf,
        server: String,
        problem: String,
    },
}

/// Read `<cwd>/.mcp.json`; no file means no servers. An entry with a string `url`
/// is HTTP (whatever its `type` says), one with a string `command` is stdio and
/// runs in `cwd`; anything else is skipped.
pub async fn load_project_mcp_config(
    fs: &dyn Fs,
    cwd: &Path,
) -> Result<Vec<McpServerConfig>, McpConfigError> {
    let path = project_mcp_path(cwd);
    let read = read_text_if_present(fs, &path).await;
    let Some(text) = read.map_err(|source| McpConfigError::Read {
        path: path.clone(),
        source,
    })?
    else {
        return Ok(Vec::new());
    };
    let parsed: Value = serde_json::from_str(&text).map_err(|source| McpConfigError::Parse {
        path: path.clone(),
        source,
    })?;
    let Some(servers) = parsed.get("mcpServers").and_then(Value::as_object) else {
        return Ok(Vec::new());
    };
    let mut out = Vec::new();
    for (name, raw) in servers {
        let invalid = |problem: String| McpConfigError::Invalid {
            path: path.clone(),
            server: name.clone(),
            problem,
        };
        if let Some(transport) = parse_server(raw, cwd).map_err(invalid)? {
            out.push(McpServerConfig {
                name: name.clone(),
                transport,
            });
        }
    }
    Ok(out)
}

fn parse_server(raw: &Value, cwd: &Path) -> Result<Option<McpTransport>, String> {
    if let Some(url) = raw.get("url").and_then(Value::as_str) {
        return Ok(Some(McpTransport::Http {
            url: url.to_string(),
            headers: string_map(raw, "headers")?,
        }));
    }
    if let Some(command) = raw.get("command").and_then(Value::as_str) {
        return Ok(Some(McpTransport::Stdio {
            command: command.to_string(),
            args: string_list(raw, "args")?,
            env: string_map(raw, "env")?,
            cwd: Some(cwd.to_path_buf()),
        }));
    }
    Ok(None)
}

fn string_list(raw: &Value, key: &str) -> Result<Vec<String>, String> {
    match raw.get(key) {
        None | Some(Value::Null) => Ok(Vec::new()),
        Some(Value::Array(items)) => items
            .iter()
            .map(|v| v.as_str().map(str::to_string))
            .collect::<Option<_>>()
            .ok_or_else(|| format!("\"{key}\" must be an array of strings")),
        Some(_) => Err(format!("\"{key}\" must be an array of strings")),
    }
}

fn string_map(raw: &Value, key: &str) -> Result<BTreeMap<String, String>, String> {
    let bad = || format!("\"{key}\" must be an object of strings");
    match raw.get(key) {
        None | Some(Value::Null) => Ok(BTreeMap::new()),
        Some(Value::Object(map)) => strings_of(map).ok_or_else(bad),
        Some(_) => Err(bad()),
    }
}

fn strings_of(map: &Map<String, Value>) -> Option<BTreeMap<String, String>> {
    map.iter()
        .map(|(k, v)| Some((k.clone(), v.as_str()?.to_string())))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use kern_core::fs::MemFs;
    use serde_json::json;

    const CWD: &str = "/p";

    fn fs_with(config: Value) -> MemFs {
        MemFs::with_files([("/p/.mcp.json", config.to_string())])
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
        let cfg = load_project_mcp_config(&fs, Path::new(CWD)).await.unwrap();
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

    #[tokio::test]
    async fn no_file_means_no_servers() {
        let cfg = load_project_mcp_config(&MemFs::new(), Path::new(CWD)).await;
        assert!(cfg.unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_file_without_servers_means_no_servers() {
        let fs = fs_with(json!({"other": 1}));
        let cfg = load_project_mcp_config(&fs, Path::new(CWD)).await;
        assert!(cfg.unwrap().is_empty());
    }

    #[tokio::test]
    async fn broken_json_is_an_error_the_host_can_report() {
        let fs = MemFs::with_files([("/p/.mcp.json", "{not json")]);
        let err = load_project_mcp_config(&fs, Path::new(CWD)).await;
        assert!(matches!(err, Err(McpConfigError::Parse { .. })));
    }

    #[tokio::test]
    async fn an_argv_that_is_not_all_strings_is_refused_naming_the_server() {
        let fs = fs_with(json!({"mcpServers": {"s": {"command": "x", "args": ["--port", 1]}}}));
        let err = load_project_mcp_config(&fs, Path::new(CWD)).await;
        assert!(matches!(err, Err(McpConfigError::Invalid { server, .. }) if server == "s"));
    }
}
