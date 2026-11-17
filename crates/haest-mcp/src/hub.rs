//! The hub: connects servers, holds the connections, hands out their tools, and
//! shuts every server down on [`McpHub::close`] -- or when the last clone is dropped.

mod failure;
mod start;

use std::{
    fmt,
    sync::{Arc, Mutex, MutexGuard},
    time::Duration,
};

use futures::future::join_all;
use haest_core::{ChildEnv, Tools, error::report, tool::ToolSource};
use rmcp::{RoleClient, model::ClientConfig, service::RunningService};

use crate::config::McpServerConfig;
pub use failure::{McpFailure, McpStartError};
use start::{Started, admit, close_quietly, start_config};

/// How long a server may take to start and answer MCP's `initialize`, and
/// again to list its tools: 15 seconds each. Past it, the server is a failure.
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
/// How long one tool call may run: 10 minutes. Past it, the call fails and is
/// cancelled on the server.
pub const CALL_TIMEOUT: Duration = Duration::from_secs(10 * 60);
/// How long closing may wait for one server; a server that hangs is left to die alone.
const CLOSE_TIMEOUT: Duration = Duration::from_secs(10);

type Client = RunningService<RoleClient, ClientConfig>;
type Log = Arc<dyn Fn(&str) + Send + Sync>;

struct Server {
    name: String,
    client: Client,
    tools: Tools,
}

/// What every clone of a hub shares: the connections and the failures.
#[derive(Default)]
struct Shared {
    servers: Mutex<Vec<Server>>,
    failures: Mutex<Vec<McpFailure>>,
}

/// The MCP servers a conversation is connected to, and their tools.
///
/// Make one ([`McpHub::new`]; [`with_env`](McpHub::with_env) gives stdio
/// servers an environment, [`with_log`](McpHub::with_log) hears one line per
/// server), [`connect`](McpHub::connect) servers to it, and hand it to the
/// loop as a [`ToolSource`]: the loop asks it for the tools before every
/// model call, so servers connected later show up on the next one. A server
/// that fails is kept in [`failures`](McpHub::failures), never returned as an
/// error.
///
/// A hub is a cheap handle: clones share the servers and the failures (each
/// keeps the environment and log it was cloned with, for the servers it
/// connects). [`close`](McpHub::close) shuts every server down; dropping the
/// last clone does too, without waiting. The crate docs have a whole example.
#[derive(Clone, Default)]
pub struct McpHub {
    shared: Arc<Shared>,
    /// What a stdio server's process gets, under its own `env`; nothing else.
    env: ChildEnv,
    log: Option<Log>,
}

impl McpHub {
    /// A hub with no servers, whose stdio servers get their own `env` and
    /// nothing else, not even `PATH` (haest reads nothing from the process
    /// environment it was not handed).
    ///
    /// So a server started by a bare name (`npx`, `uvx`) is looked up only in
    /// the system's default directories (`/usr/bin`, `/bin`), and runs with no
    /// `PATH` of its own; one not found there fails saying the PATH is
    /// missing. A host that starts servers by name hands `PATH` in with
    /// [`with_env`](Self::with_env) (or in a server's own `env`).
    pub fn new() -> Self {
        Self::default()
    }

    /// Stdio servers this hub connects get `env`, with each one's own `env`
    /// over it. A host passes the variables it means a server to have
    /// (haest-config's `env_passthrough` is one way to pick them).
    #[must_use]
    pub fn with_env(mut self, env: ChildEnv) -> Self {
        self.env = env;
        self
    }

    /// `log` hears one line per server this hub connects: how many tools it
    /// has, or why it failed. Without one, the lines go nowhere; the failures
    /// are in [`failures`](Self::failures) either way.
    #[must_use]
    pub fn with_log(mut self, log: impl Fn(&str) + Send + Sync + 'static) -> Self {
        self.log = Some(Arc::new(log));
        self
    }

    /// Connect every server in `configs`, all at once, and wait until each has
    /// connected and listed its tools or failed.
    ///
    /// One that fails is logged ([`with_log`](Self::with_log)) and recorded in
    /// [`failures`](Self::failures), never returned as an error. The tools are
    /// added in the order of `configs` (not the order the servers answered),
    /// so when two would get the same name the earlier server keeps the plain
    /// one ([`mcp_tool_name`](crate::mcp_tool_name)).
    ///
    /// Dropping the future before it finishes adds no server; the ones that
    /// had started are shut down as they are dropped.
    ///
    /// # Examples
    ///
    /// A server that cannot start is a failure to report, not an error:
    ///
    /// ```
    /// use haest_core::error::report;
    /// use haest_mcp::{McpHub, McpServerConfig, McpStartError, McpTransport};
    ///
    /// # #[tokio::main(flavor = "current_thread")]
    /// # async fn main() {
    /// let hub = McpHub::new().with_log(|line| println!("{line}"));
    /// let broken = McpServerConfig::new("broken", McpTransport::stdio("/no/such/program"));
    /// hub.connect(&[broken]).await;
    ///
    /// assert!(hub.tools().is_empty());
    /// let failed = &hub.failures()[0];
    /// assert_eq!(failed.name(), "broken");
    /// assert!(matches!(failed.error(), McpStartError::Spawn { .. }));
    /// println!("MCP server {} did not start: {}", failed.name(), report(failed.error()));
    /// # }
    /// ```
    pub async fn connect(&self, configs: &[McpServerConfig]) {
        let started = join_all(configs.iter().map(|cfg| async move {
            let res = start_config(cfg, &self.env).await;
            self.settle(&cfg.name, res)
        }))
        .await;
        let mut servers = self.lock_servers();
        for (cfg, started) in configs.iter().zip(started) {
            if let Some(started) = started {
                admit(&mut servers, &cfg.name, started);
            }
        }
    }

    /// PLUG POINT: connect a server over any rmcp transport (an in-process
    /// server, a socket the host opened), with the same naming and failure rules.
    ///
    /// `name` is the server's name, as [`McpServerConfig::name`] would be. The
    /// transport is anything rmcp can serve a client over: an
    /// `AsyncRead + AsyncWrite` pair such as one end of `tokio::io::duplex`, a
    /// child process, an HTTP client transport. Its tools are added as soon as
    /// it has listed them; a failure goes to [`failures`](Self::failures) and
    /// the log, as with [`connect`](Self::connect). The crate docs' first
    /// example connects a server in the same process this way.
    ///
    /// Behind the `rmcp` feature: the transport is an rmcp type, so this puts
    /// rmcp in the host's API.
    #[cfg(any(test, feature = "rmcp"))]
    pub async fn connect_transport<T, E, A>(&self, name: &str, transport: T)
    where
        T: rmcp::transport::IntoTransport<RoleClient, E, A>,
        E: std::error::Error + Send + Sync + 'static,
    {
        let res = start::start(transport).await;
        if let Some(started) = self.settle(name, res) {
            admit(&mut self.lock_servers(), name, started);
        }
    }

    fn settle(&self, name: &str, res: Result<Started, McpStartError>) -> Option<Started> {
        let line = match &res {
            Ok(started) => format!("mcp {name}: {} tools", started.listed.len()),
            Err(error) => format!("mcp {name}: FAILED {}", report(error)),
        };
        if let Some(log) = &self.log {
            log(&line);
        }
        match res {
            Ok(started) => Some(started),
            Err(error) => {
                let failure = McpFailure::new(name, error);
                self.lock_failures().push(failure);
                None
            }
        }
    }

    fn lock_servers(&self) -> MutexGuard<'_, Vec<Server>> {
        self.shared.servers.lock().unwrap()
    }

    fn lock_failures(&self) -> MutexGuard<'_, Vec<McpFailure>> {
        self.shared.failures.lock().unwrap()
    }

    /// Every connected server's tools: the servers in the order they were
    /// added, each one's tools in the order it listed them. Empty after
    /// [`close`](Self::close).
    pub fn tools(&self) -> Tools {
        self.lock_servers()
            .iter()
            .flat_map(|s| s.tools.iter().cloned())
            .collect()
    }

    /// Every server that did not connect since the hub was made, in the order
    /// they failed.
    pub fn failures(&self) -> Vec<McpFailure> {
        self.lock_failures().clone()
    }

    /// Shut every server down (a stdio server's process included) and forget
    /// them; their tools fail from now on. Dropping the last clone of the hub
    /// does the same without waiting.
    ///
    /// Waits up to 10 seconds for each server, all at once; one that hangs is
    /// left to die on its own. The hub stays usable: servers connected after
    /// are offered as before.
    pub async fn close(&self) {
        let servers = std::mem::take(&mut *self.lock_servers());
        join_all(servers.into_iter().map(|s| close_quietly(s.client))).await;
    }
}

impl ToolSource for McpHub {
    fn tools(&self) -> Tools {
        McpHub::tools(self)
    }
}

/// The servers by name, the failures, and the names of the variables a
/// stdio server gets (never their values).
#[expect(
    clippy::missing_fields_in_debug,
    reason = "`shared` is shown as what it holds: the servers and the failures"
)]
impl fmt::Debug for McpHub {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let servers: Vec<String> = self.lock_servers().iter().map(|s| s.name.clone()).collect();
        f.debug_struct("McpHub")
            .field("servers", &servers)
            .field("failures", &self.failures())
            .field("env", &self.env)
            .field("log", &self.log.is_some())
            .finish()
    }
}

#[cfg(test)]
mod tests;
