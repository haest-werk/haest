//! Which MCP servers to connect: a server's name and how to reach it.

use std::{collections::BTreeMap, path::PathBuf};

/// One MCP server to connect: its name and how to reach it.
///
/// [`McpHub::connect`](crate::McpHub::connect) takes a list of them. Build one
/// with [`McpServerConfig::stdio`] or [`McpServerConfig::http`] and the
/// `with_*` methods (arguments, variables and a dir for a program, headers
/// for HTTP), or read a project's with
/// [`load_project_mcp_config`](crate::load_project_mcp_config).
///
/// ```
/// use std::collections::BTreeMap;
///
/// use haest_mcp::{McpServerConfig, McpTransport};
///
/// let github = McpServerConfig::stdio("github", "github-mcp-server")
///     .with_args(["stdio"])
///     .with_env("GITHUB_TOKEN", "ghp_example");
/// let McpTransport::Stdio { args, env, .. } = &github.transport else {
///     unreachable!("stdio() makes a stdio server")
/// };
/// assert_eq!(args, &["stdio"]);
/// assert_eq!(env["GITHUB_TOKEN"], "ghp_example");
///
/// let docs = McpServerConfig::http("docs", "https://docs.example.com/mcp")
///     .with_header("Authorization", "Bearer t0ken");
/// let McpTransport::Http { headers, .. } = &docs.transport else {
///     unreachable!("http() makes an HTTP server")
/// };
/// assert_eq!(headers, &BTreeMap::from([("Authorization".into(), "Bearer t0ken".into())]));
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
        /// process is given (with none, only in the system's default
        /// directories: see [`McpHub::new`](crate::McpHub::new)).
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
    /// variables of its own, run in this process's working dir; the `with_*`
    /// methods add them.
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

    /// `args` after the ones a stdio server already has.
    ///
    /// # Panics
    ///
    /// On an HTTP server: only a program takes arguments.
    pub fn with_args<S: Into<String>>(mut self, args: impl IntoIterator<Item = S>) -> Self {
        let McpTransport::Stdio { args: have, .. } = &mut self.transport else {
            panic!("with_args on HTTP MCP server \"{}\"", self.name);
        };
        have.extend(args.into_iter().map(Into::into));
        self
    }

    /// The variable `name` set to `value` for this stdio server alone, over
    /// the hub's environment.
    ///
    /// # Panics
    ///
    /// On an HTTP server: only a program has an environment.
    pub fn with_env(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        let McpTransport::Stdio { env, .. } = &mut self.transport else {
            panic!("with_env on HTTP MCP server \"{}\"", self.name);
        };
        env.insert(name.into(), value.into());
        self
    }

    /// The dir this stdio server runs in.
    ///
    /// # Panics
    ///
    /// On an HTTP server: only a program runs in a dir.
    pub fn with_cwd(mut self, dir: impl Into<PathBuf>) -> Self {
        let McpTransport::Stdio { cwd, .. } = &mut self.transport else {
            panic!("with_cwd on HTTP MCP server \"{}\"", self.name);
        };
        *cwd = Some(dir.into());
        self
    }

    /// The header `name` with `value` on every request to this HTTP server.
    ///
    /// # Panics
    ///
    /// On a stdio server: only HTTP has headers.
    pub fn with_header(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        let McpTransport::Http { headers, .. } = &mut self.transport else {
            panic!("with_header on stdio MCP server \"{}\"", self.name);
        };
        headers.insert(name.into(), value.into());
        self
    }
}

#[cfg(test)]
mod tests;
