//! The project's `.mcp.json` (Claude Code format):
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

use crate::config::{HttpTransport, McpServerConfig, McpTransport, StdioTransport};

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

/// Why `.mcp.json` could not be used. A host reports it and goes on without the file.
///
/// Every variant's message starts with the file's path; the io or JSON error
/// is the source, so show it with its causes (`haest_core::error::report`).
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum McpConfigError {
    /// The file is there but could not be read.
    #[error("{}: cannot read it", path.display())]
    Read {
        /// The `.mcp.json` it is about.
        path: PathBuf,
        /// What reading it failed with.
        source: std::io::Error,
    },
    /// The file is not JSON.
    #[error("{} is not valid JSON", path.display())]
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
#[non_exhaustive]
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
/// # async fn main() -> Result<(), haest_core::error::BoxError> {
/// let fs = MemFs::from_files([(
///     "/repo/.mcp.json",
///     r#"{"mcpServers": {
///         "github": {"command": "github-mcp-server", "args": ["stdio"],
///                    "env": {"GITHUB_TOKEN": "${GH_TOKEN}"}},
///         "docs": {"type": "http", "url": "https://docs.example.com/mcp",
///                  "headers": {"Authorization": "Bearer ${DOCS_TOKEN}"}}
///     }}"#,
/// )]);
/// let vars = |name: &str| (name == "GH_TOKEN").then(|| "ghp_example".to_string());
/// let project = load_project_mcp_config(&fs, Path::new("/repo"), vars).await?;
///
/// let github = project.servers.iter().find(|s| s.name == "github").ok_or("no github")?;
/// let McpTransport::Stdio(stdio) = &github.transport else {
///     return Err("a command is a stdio server".into());
/// };
/// assert_eq!(stdio.env["GITHUB_TOKEN"], "ghp_example");
/// assert_eq!(stdio.cwd.as_deref(), Some(Path::new("/repo")));
///
/// // DOCS_TOKEN has no value: the header goes as written, and a note says so.
/// assert_eq!(project.servers.len(), 2);
/// assert_eq!(project.notes.len(), 1);
/// # Ok(())
/// # }
/// ```
pub async fn load_project_mcp_config(
    fs: &dyn Fs,
    cwd: &Path,
    vars: impl Fn(&str) -> Option<String>,
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
            vars: &vars,
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
        return Ok(Some(McpTransport::Http(HttpTransport {
            url: fill.text(url),
            headers: fill.map(string_map(raw, "headers")?),
        })));
    }
    if let Some(command) = raw.get("command").and_then(Value::as_str) {
        return Ok(Some(McpTransport::Stdio(StdioTransport {
            command: fill.text(command),
            args: fill.list(string_list(raw, "args")?),
            env: fill.map(string_map(raw, "env")?),
            cwd: Some(cwd.to_path_buf()),
        })));
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
