//! The hub: connects servers, holds the connections, hands out their tools, and
//! shuts every server down on [`McpHub::close`] -- or when it is dropped.

mod failure;
mod start;

use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

use futures::future::join_all;
use haest_core::{
    ChildEnv,
    error::report,
    tool::{Tool, ToolSource},
};
use rmcp::{RoleClient, model::ClientConfig, service::RunningService, transport::IntoTransport};

use crate::config::McpServerConfig;
pub use failure::{McpFailure, McpStartError};
use start::{Started, admit, close_quietly, start, start_config};

/// How long a server may take to start and answer MCP's `initialize`, and
/// again to list its tools: 15 seconds each. Past it, the server is a failure.
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
/// How long one tool call may run: 10 minutes. Past it, the call fails and is
/// cancelled on the server.
pub const CALL_TIMEOUT: Duration = Duration::from_secs(10 * 60);
/// How long closing may wait for one server; a server that hangs is left to die alone.
const CLOSE_TIMEOUT: Duration = Duration::from_secs(10);

type Client = RunningService<RoleClient, ClientConfig>;

struct Server {
    client: Client,
    tools: Vec<Arc<dyn Tool>>,
}

/// The MCP servers a conversation is connected to, and their tools.
///
/// Make one ([`McpHub::new`], or [`McpHub::with_env`] to give stdio servers an
/// environment), [`connect`](McpHub::connect) servers to it, and share it
/// (`Arc<McpHub>`) as the loop's [`ToolSource`]: the loop asks it for the tools
/// before every model call, so servers connected later show up on the next
/// one. A server that fails is kept in [`failures`](McpHub::failures), never
/// returned as an error.
///
/// [`close`](McpHub::close) shuts every server down; dropping the hub does too,
/// without waiting. The crate docs have a whole example.
#[derive(Default)]
pub struct McpHub {
    servers: Mutex<Vec<Server>>,
    failures: Mutex<Vec<McpFailure>>,
    /// What a stdio server's process gets, under its own `env`; nothing else.
    env: ChildEnv,
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

    /// A hub with no servers, whose stdio servers get `env`, with each one's
    /// own `env` over it. A host passes the variables it means a server to
    /// have (haest-config's `env_passthrough` is one way to pick them).
    pub fn with_env(env: ChildEnv) -> Self {
        Self {
            env,
            ..Self::default()
        }
    }

    /// Connect every server in `configs`, all at once, and wait until each has
    /// connected and listed its tools or failed.
    ///
    /// One that fails is logged and recorded in [`failures`](Self::failures),
    /// never returned as an error. `log` gets one line per server: how many
    /// tools it has, or why it failed. The tools are added in the order of
    /// `configs` (not the order the servers answered), so when two would get
    /// the same name the earlier server keeps the plain one
    /// ([`mcp_tool_name`](crate::mcp_tool_name)).
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
    /// let hub = McpHub::new();
    /// let broken = McpServerConfig::new("broken", McpTransport::stdio("/no/such/program"));
    /// hub.connect(&[broken], &|line| println!("{line}")).await;
    ///
    /// assert!(hub.tools().is_empty());
    /// let failed = &hub.failures()[0];
    /// assert_eq!(failed.name, "broken");
    /// assert!(matches!(*failed.error, McpStartError::Spawn { .. }));
    /// println!("MCP server {} did not start: {}", failed.name, report(&*failed.error));
    /// # }
    /// ```
    pub async fn connect(&self, configs: &[McpServerConfig], log: &(dyn Fn(&str) + Sync)) {
        let started = join_all(configs.iter().map(|cfg| async move {
            let res = start_config(cfg, &self.env).await;
            self.settle(&cfg.name, res, log)
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
    /// `log`, as with [`connect`](Self::connect). The crate docs' first
    /// example connects a server in the same process this way.
    pub async fn connect_transport<T, E, A>(
        &self,
        name: &str,
        transport: T,
        log: &(dyn Fn(&str) + Sync),
    ) where
        T: IntoTransport<RoleClient, E, A>,
        E: std::error::Error + Send + Sync + 'static,
    {
        let res = start(transport).await;
        if let Some(started) = self.settle(name, res, log) {
            admit(&mut self.lock_servers(), name, started);
        }
    }

    fn settle(
        &self,
        name: &str,
        res: Result<Started, McpStartError>,
        log: &(dyn Fn(&str) + Sync),
    ) -> Option<Started> {
        match res {
            Ok(started) => {
                log(&format!("mcp {name}: {} tools", started.listed.len()));
                Some(started)
            }
            Err(error) => {
                log(&format!("mcp {name}: FAILED {}", report(&error)));
                let failure = McpFailure {
                    name: name.to_string(),
                    error: Arc::new(error),
                };
                self.failures.lock().unwrap().push(failure);
                None
            }
        }
    }

    fn lock_servers(&self) -> std::sync::MutexGuard<'_, Vec<Server>> {
        self.servers.lock().unwrap()
    }

    /// Every connected server's tools: the servers in the order they were
    /// added, each one's tools in the order it listed them. Empty after
    /// [`close`](Self::close).
    pub fn tools(&self) -> Vec<Arc<dyn Tool>> {
        self.lock_servers()
            .iter()
            .flat_map(|s| s.tools.iter().cloned())
            .collect()
    }

    /// Every server that did not connect since the hub was made, in the order
    /// they failed.
    pub fn failures(&self) -> Vec<McpFailure> {
        self.failures.lock().unwrap().clone()
    }

    /// Shut every server down (a stdio server's process included) and forget
    /// them; their tools fail from now on. Dropping the hub does the same
    /// without waiting.
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
    fn tools(&self) -> Vec<Arc<dyn Tool>> {
        McpHub::tools(self)
    }
}

#[cfg(test)]
mod tests;
