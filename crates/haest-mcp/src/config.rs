//! Which MCP servers to connect, and the project's `.mcp.json` (Claude Code format):
//! `{ "mcpServers": { "<name>": {command, args, env} | {type: "http", url, headers} } }`.

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

use haest_core::{
    expand::{expand_vars, unset_note},
    fs::{Fs, read_text_if_present},
};
use serde_json::Value;

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

/// What a project's `.mcp.json` gave.
#[derive(Debug, Default)]
pub struct ProjectMcp {
    pub servers: Vec<McpServerConfig>,
    /// One per server that names a variable with no value and no default
    /// (passed as written, as Claude Code does): the host shows them.
    pub notes: Vec<String>,
}

/// Read `<cwd>/.mcp.json`; no file means no servers. An entry with a string `url`
/// is HTTP (whatever its `type` says), one with a string `command` is stdio and
/// runs in `cwd`; anything else is skipped. `${NAME}` and `${NAME:-default}` in
/// a `command`, `args`, `env`, `url` or `headers` value are filled in from
/// `vars` ([`expand_vars`]).
pub async fn load_project_mcp_config(
    fs: &dyn Fs,
    cwd: &Path,
    vars: &dyn Fn(&str) -> Option<String>,
) -> Result<ProjectMcp, McpConfigError> {
    let path = project_mcp_path(cwd);
    let read = read_text_if_present(fs, &path).await;
    let Some(text) = read.map_err(|source| McpConfigError::Read {
        path: path.clone(),
        source,
    })?
    else {
        return Ok(ProjectMcp::default());
    };
    let parsed: Value = serde_json::from_str(&text).map_err(|source| McpConfigError::Parse {
        path: path.clone(),
        source,
    })?;
    let Some(servers) = parsed.get("mcpServers").and_then(Value::as_object) else {
        return Ok(ProjectMcp::default());
    };
    let mut out = ProjectMcp::default();
    for (name, raw) in servers {
        let invalid = |problem: String| McpConfigError::Invalid {
            path: path.clone(),
            server: name.clone(),
            problem,
        };
        let mut fill = Fill {
            vars,
            unset: Vec::new(),
        };
        if let Some(transport) = parse_server(raw, cwd, &mut fill).map_err(invalid)? {
            out.servers.push(McpServerConfig {
                name: name.clone(),
                transport,
            });
        }
        if !fill.unset.is_empty() {
            out.notes.push(format!(
                "{}: server \"{name}\" {}: passed as written",
                path.display(),
                unset_note(&fill.unset)
            ));
        }
    }
    Ok(out)
}

/// One server's values with their variables filled in, and the names left unset.
struct Fill<'a> {
    vars: &'a dyn Fn(&str) -> Option<String>,
    unset: Vec<String>,
}

impl Fill<'_> {
    fn text(&mut self, text: &str) -> String {
        let (filled, unset) = expand_vars(text, self.vars);
        for name in unset {
            if !self.unset.contains(&name) {
                self.unset.push(name);
            }
        }
        filled
    }

    fn list(&mut self, items: Vec<String>) -> Vec<String> {
        items.iter().map(|s| self.text(s)).collect()
    }

    fn map(&mut self, map: BTreeMap<String, String>) -> BTreeMap<String, String> {
        map.into_iter().map(|(k, v)| (k, self.text(&v))).collect()
    }
}

fn parse_server(
    raw: &Value,
    cwd: &Path,
    fill: &mut Fill<'_>,
) -> Result<Option<McpTransport>, String> {
    if let Some(url) = raw.get("url").and_then(Value::as_str) {
        return Ok(Some(McpTransport::Http {
            url: fill.text(url),
            headers: fill.map(string_map(raw, "headers")?),
        }));
    }
    if let Some(command) = raw.get("command").and_then(Value::as_str) {
        return Ok(Some(McpTransport::Stdio {
            command: fill.text(command),
            args: fill.list(string_list(raw, "args")?),
            env: fill.map(string_map(raw, "env")?),
            cwd: Some(cwd.to_path_buf()),
        }));
    }
    Ok(None)
}

fn string_list(raw: &Value, key: &str) -> Result<Vec<String>, String> {
    let bad = || format!("\"{key}\" must be an array of strings");
    match raw.get(key) {
        None | Some(Value::Null) => Ok(Vec::new()),
        Some(Value::Array(items)) => items
            .iter()
            .map(|v| v.as_str().map(str::to_string))
            .collect::<Option<_>>()
            .ok_or_else(bad),
        Some(_) => Err(bad()),
    }
}

fn string_map(raw: &Value, key: &str) -> Result<BTreeMap<String, String>, String> {
    let bad = || format!("\"{key}\" must be an object of strings");
    match raw.get(key) {
        None | Some(Value::Null) => Ok(BTreeMap::new()),
        Some(Value::Object(map)) => map
            .iter()
            .map(|(k, v)| Some((k.clone(), v.as_str()?.to_string())))
            .collect::<Option<_>>()
            .ok_or_else(bad),
        Some(_) => Err(bad()),
    }
}

#[cfg(test)]
mod tests;
