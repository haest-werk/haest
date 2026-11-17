//! Which MCP servers to connect: a server's name and how to reach it.

use std::{collections::BTreeMap, fmt, path::PathBuf};

/// What a header's or a variable's value shows as in Debug output.
const REDACTED: &str = "<redacted>";

/// Headers or variables as Debug output shows them: each name, never its
/// value (a token rides in one).
struct Names<'a>(&'a BTreeMap<String, String>);

impl fmt::Debug for Names<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let names = self.0.keys().map(|name| (name, REDACTED));
        f.debug_map().entries(names).finish()
    }
}

/// One MCP server to connect: its name and how to reach it.
///
/// [`McpHub::connect`](crate::McpHub::connect) takes a list of them. Build one
/// with [`McpServerConfig::new`] over a transport ([`McpTransport::stdio`] or
/// [`McpTransport::http`] and their `with_*` methods), or read a project's
/// with [`load_project_mcp_config`](crate::load_project_mcp_config).
///
/// ```
/// use std::collections::BTreeMap;
///
/// use haest_mcp::{McpServerConfig, McpTransport};
///
/// let github = McpServerConfig::new(
///     "github",
///     McpTransport::stdio("github-mcp-server")
///         .with_args(["stdio"])
///         .with_env_var("GITHUB_TOKEN", "ghp_example"),
/// );
/// let McpTransport::Stdio(stdio) = &github.transport else {
///     unreachable!("stdio() makes a stdio server")
/// };
/// assert_eq!(stdio.args, ["stdio"]);
/// assert_eq!(stdio.env["GITHUB_TOKEN"], "ghp_example");
///
/// let docs = McpServerConfig::new(
///     "docs",
///     McpTransport::http("https://docs.example.com/mcp").with_header("Authorization", "Bearer t0ken"),
/// );
/// let McpTransport::Http(http) = &docs.transport else {
///     unreachable!("http() makes an HTTP server")
/// };
/// assert_eq!(http.headers, BTreeMap::from([("Authorization".into(), "Bearer t0ken".into())]));
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpServerConfig {
    /// The server's name: the `<server>` in `mcp__<server>__<tool>`, and the
    /// name a failure and a log line give.
    pub name: String,
    /// How to reach it.
    pub transport: McpTransport,
}

impl McpServerConfig {
    /// The server `name`, reached through `transport`: a [`StdioTransport`],
    /// an [`HttpTransport`] or an [`McpTransport`].
    pub fn new(name: impl Into<String>, transport: impl Into<McpTransport>) -> Self {
        Self {
            name: name.into(),
            transport: transport.into(),
        }
    }
}

/// How to reach an MCP server: streamable HTTP, or a child process on stdio.
///
/// A host that reaches a server some other way (a server in the same process,
/// a socket it opened) connects it with `McpHub::connect_transport` (feature
/// `rmcp`) instead.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum McpTransport {
    /// Streamable HTTP.
    Http(HttpTransport),
    /// A child process speaking MCP on its stdin and stdout.
    Stdio(StdioTransport),
}

impl McpTransport {
    /// A streamable-HTTP server at `url`, with no headers; [`HttpTransport::with_header`] adds them.
    pub fn http(url: impl Into<String>) -> HttpTransport {
        HttpTransport {
            url: url.into(),
            headers: BTreeMap::new(),
        }
    }

    /// The program `command`, with no arguments, no variables of its own,
    /// run in this process's working dir; the [`StdioTransport`] `with_*`
    /// methods add them.
    pub fn stdio(command: impl Into<String>) -> StdioTransport {
        StdioTransport {
            command: command.into(),
            args: Vec::new(),
            env: BTreeMap::new(),
            cwd: None,
        }
    }
}

/// A streamable-HTTP MCP server; `headers` ride on every request (an
/// `Authorization` included). Start from [`McpTransport::http`].
///
/// Its Debug output names the headers and never shows their values.
#[derive(Clone, PartialEq, Eq)]
pub struct HttpTransport {
    /// The server's endpoint, e.g. `https://example.com/mcp`.
    pub url: String,
    /// Header names and values sent with every request. One that HTTP
    /// cannot carry fails this server before any request goes out.
    pub headers: BTreeMap<String, String>,
}

impl HttpTransport {
    /// The header `name` with `value` on every request (one already set is replaced).
    pub fn with_header(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.headers.insert(name.into(), value.into());
        self
    }
}

impl fmt::Debug for HttpTransport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HttpTransport")
            .field("url", &self.url)
            .field("headers", &Names(&self.headers))
            .finish()
    }
}

impl From<HttpTransport> for McpTransport {
    fn from(http: HttpTransport) -> Self {
        Self::Http(http)
    }
}

/// An MCP server that is a child process speaking MCP on its stdin and
/// stdout. Start from [`McpTransport::stdio`].
///
/// It gets the hub's environment ([`McpHub::with_env`](crate::McpHub::with_env))
/// with `env` laid over it, and nothing else of this process's environment.
/// Its stderr is discarded, and it is killed when its connection drops.
///
/// Its Debug output names the variables in `env` and never shows their values.
#[derive(Clone, PartialEq, Eq)]
pub struct StdioTransport {
    /// The program to run: a path, or a name looked up on the `PATH` the
    /// process is given (with none, only in the system's default
    /// directories: see [`McpHub::new`](crate::McpHub::new)).
    pub command: String,
    /// Its arguments.
    pub args: Vec<String>,
    /// Variables for this server alone, laid over the hub's environment.
    pub env: BTreeMap<String, String>,
    /// The dir it runs in; None = this process's working dir.
    pub cwd: Option<PathBuf>,
}

impl StdioTransport {
    /// `args` after the ones it already has.
    pub fn with_args<S: Into<String>>(mut self, args: impl IntoIterator<Item = S>) -> Self {
        self.args.extend(args.into_iter().map(Into::into));
        self
    }

    /// The variable `name` set to `value` for this server alone, over the
    /// hub's environment (one already set is replaced).
    pub fn with_env_var(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.env.insert(name.into(), value.into());
        self
    }

    /// The dir it runs in.
    pub fn with_cwd(mut self, dir: impl Into<PathBuf>) -> Self {
        self.cwd = Some(dir.into());
        self
    }
}

impl fmt::Debug for StdioTransport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("StdioTransport")
            .field("command", &self.command)
            .field("args", &self.args)
            .field("env", &Names(&self.env))
            .field("cwd", &self.cwd)
            .finish()
    }
}

impl From<StdioTransport> for McpTransport {
    fn from(stdio: StdioTransport) -> Self {
        Self::Stdio(stdio)
    }
}

#[cfg(test)]
mod tests;
