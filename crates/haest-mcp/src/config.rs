//! Which MCP servers to connect, and the project's `.mcp.json` (Claude Code format):
//! `{ "mcpServers": { "<name>": {command, args, env} | {type: "http", url, headers} } }`.

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

use haest_core::{
    __private::{expand_vars, unset_note},
    fs::{Fs, read_text_if_present},
};
use serde_json::Value;

/// Where a project keeps its MCP servers: `<cwd>/.mcp.json`.
///
/// ```
/// # use std::path::Path;
/// assert_eq!(
///     haest_mcp::project_mcp_path(Path::new("/repo")),
///     Path::new("/repo/.mcp.json")
/// );
/// ```
pub fn project_mcp_path(cwd: &Path) -> PathBuf {
    cwd.join(".mcp.json")
}

/// One MCP server to connect: its name and how to reach it.
///
/// [`McpHub::connect`](crate::McpHub::connect) takes a list of them. Build one
/// with [`McpServerConfig::stdio`] or [`McpServerConfig::http`], write the
/// struct out to give a stdio server arguments or an HTTP server headers, or
/// read a project's with [`load_project_mcp_config`].
///
/// ```
/// use std::collections::BTreeMap;
///
/// use haest_mcp::{McpServerConfig, McpTransport};
///
/// let docs = McpServerConfig {
///     name: "docs".into(),
///     transport: McpTransport::Http {
///         url: "https://docs.example.com/mcp".into(),
///         headers: BTreeMap::from([("Authorization".into(), "Bearer t0ken".into())]),
///     },
/// };
/// assert_eq!(McpServerConfig::http("docs", "https://docs.example.com/mcp").name, docs.name);
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpServerConfig {
    /// The server's name: the `<server>` in `mcp__<server>__<tool>`, and the
    /// name a failure and a log line give.
    pub name: String,
    /// How to reach it.
    pub transport: McpTransport,
}

/// How to reach an MCP server: streamable HTTP, or a child process on stdio.
///
/// A host that reaches a server some other way (a server in the same process,
/// a socket it opened) connects it with
/// [`McpHub::connect_transport`](crate::McpHub::connect_transport) instead.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum McpTransport {
    /// Streamable HTTP; `headers` ride on every request (an `Authorization` included).
    Http {
        /// The server's endpoint, e.g. `https://example.com/mcp`.
        url: String,
        /// Header names and values sent with every request. One that HTTP
        /// cannot carry fails this server before any request goes out.
        headers: BTreeMap<String, String>,
    },
    /// A child process speaking MCP on its stdin and stdout. It gets the
    /// hub's environment ([`McpHub::with_env`](crate::McpHub::with_env)) with
    /// `env` laid over it, and nothing else of this process's environment. Its
    /// stderr is discarded, and it is killed when its connection drops.
    Stdio {
        /// The program to run: a path, or a name looked up on the `PATH` the
        /// process is given.
        command: String,
        /// Its arguments.
        args: Vec<String>,
        /// Variables for this server alone, laid over the hub's environment.
        env: BTreeMap<String, String>,
        /// The dir it runs in; None = this process's working dir.
        cwd: Option<PathBuf>,
    },
}

impl McpServerConfig {
    /// A streamable-HTTP server at `url`, with no headers.
    pub fn http(name: impl Into<String>, url: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            transport: McpTransport::Http {
                url: url.into(),
                headers: BTreeMap::new(),
            },
        }
    }

    /// A server that is the program `command`, with no arguments, no
    /// variables of its own, run in this process's working dir.
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
///
/// Every variant's message starts with the file's path.
#[derive(Debug, thiserror::Error)]
pub enum McpConfigError {
    /// The file is there but could not be read.
    #[error("{}: {source}", path.display())]
    Read {
        /// The `.mcp.json` it is about.
        path: PathBuf,
        /// What reading it failed with.
        source: std::io::Error,
    },
    /// The file is not JSON.
    #[error("{}: {source}", path.display())]
    Parse {
        /// The `.mcp.json` it is about.
        path: PathBuf,
        /// Where the JSON broke.
        source: serde_json::Error,
    },
    /// A field of a known server has the wrong type: refusing beats starting a
    /// program with an argv nobody wrote.
    #[error("{}: server \"{server}\": {problem}", path.display())]
    Invalid {
        /// The `.mcp.json` it is about.
        path: PathBuf,
        /// The entry's name under `mcpServers`.
        server: String,
        /// Which field is wrong, and what it must be.
        problem: String,
    },
}

/// What a project's `.mcp.json` gave: its servers, and what the host should
/// tell the human about them.
#[derive(Debug, Default)]
pub struct ProjectMcp {
    /// The servers it lists that haest can reach (one with a `url` or a `command`).
    pub servers: Vec<McpServerConfig>,
    /// One per server that names a variable with no value and no default
    /// (passed as written, as Claude Code does): the host shows them.
    pub notes: Vec<String>,
}

/// Read a project's `<cwd>/.mcp.json`, in Claude Code's format, through `fs`.
///
/// No file means no servers, and so does a file with no `mcpServers` object.
/// An entry with a string `url` is HTTP (whatever its `type` says) and takes
/// its `headers`; one with a string `command` is stdio, takes its `args` and
/// `env`, and runs in `cwd`; anything else is skipped. `${NAME}` and
/// `${NAME:-default}` in a `command`, `args`, `env`, `url` or `headers` value
/// are filled in from `vars` ([`expand_vars`]); a name `vars` has no value for,
/// with no default, stays as written and gets a note in
/// [`ProjectMcp::notes`].
///
/// This reads the file whoever wrote it. A host decides first whether to trust
/// the project: an `.mcp.json` starts the programs it names. haest-config's
/// loader reads it only inside `trusted_dirs`.
///
/// # Errors
///
/// [`McpConfigError::Read`] when the file is there and cannot be read,
/// [`McpConfigError::Parse`] when it is not JSON, and
/// [`McpConfigError::Invalid`] when a server's `args` is not a list of strings,
/// or its `env` or `headers` not an object of strings.
///
/// # Examples
///
/// ```
/// use std::path::Path;
///
/// use haest_core::fs::MemFs;
/// use haest_mcp::{McpTransport, load_project_mcp_config};
///
/// # #[tokio::main(flavor = "current_thread")]
/// # async fn main() {
/// let fs = MemFs::with_files([(
///     "/repo/.mcp.json",
///     r#"{"mcpServers": {
///         "github": {"command": "github-mcp-server", "args": ["stdio"],
///                    "env": {"GITHUB_TOKEN": "${GH_TOKEN}"}},
///         "docs": {"type": "http", "url": "https://docs.example.com/mcp",
///                  "headers": {"Authorization": "Bearer ${DOCS_TOKEN}"}}
///     }}"#,
/// )]);
/// let vars = |name: &str| (name == "GH_TOKEN").then(|| "ghp_example".to_string());
/// let project = load_project_mcp_config(&fs, Path::new("/repo"), &vars)
///     .await
///     .unwrap();
///
/// let github = project.servers.iter().find(|s| s.name == "github").unwrap();
/// let McpTransport::Stdio { env, cwd, .. } = &github.transport else {
///     panic!("a command is a stdio server")
/// };
/// assert_eq!(env["GITHUB_TOKEN"], "ghp_example");
/// assert_eq!(cwd.as_deref(), Some(Path::new("/repo")));
///
/// // DOCS_TOKEN has no value: the header goes as written, and a note says so.
/// assert_eq!(project.servers.len(), 2);
/// assert_eq!(project.notes.len(), 1);
/// # }
/// ```
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
