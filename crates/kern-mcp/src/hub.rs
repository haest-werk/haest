//! The hub: connects servers, holds the connections, hands out their tools, and
//! shuts every server down on [`McpHub::close`] -- or when it is dropped.

use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

use futures::future::join_all;
use kern_core::{
    ChildEnv,
    tool::{Tool, ToolSource},
};
use rmcp::{
    RoleClient, ServiceExt,
    model::{ClientCapabilities, ClientConfig, Implementation},
    service::RunningService,
    transport::IntoTransport,
};

use crate::{
    config::{McpServerConfig, McpTransport},
    tool::McpTool,
    transport,
};

/// How long a server may take to start, and again to list its tools.
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
/// How long one tool call may run.
pub const CALL_TIMEOUT: Duration = Duration::from_secs(10 * 60);
/// How long closing may wait for one server; a server that hangs is left to die alone.
const CLOSE_TIMEOUT: Duration = Duration::from_secs(10);

/// A server that did not connect, and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpFailure {
    pub name: String,
    pub error: String,
}

type Client = RunningService<RoleClient, ClientConfig>;

struct Server {
    client: Client,
    tools: Vec<Arc<dyn Tool>>,
}

/// Connected MCP servers. Share it (`Arc<McpHub>`) as the loop's [`ToolSource`]:
/// servers connected later show up on the next model call.
#[derive(Default)]
pub struct McpHub {
    servers: Mutex<Vec<Server>>,
    failures: Mutex<Vec<McpFailure>>,
    /// What a stdio server's process gets, under its own `env`; nothing else.
    env: ChildEnv,
}

fn timed_out(name: &str, what: &str, after: Duration) -> String {
    format!("MCP {name} {what} timed out after {}ms", after.as_millis())
}

async fn close_quietly(mut client: Client) {
    let _ = client.close_with_timeout(CLOSE_TIMEOUT).await;
}

/// Initialize, then list the tools; a failure closes what was opened.
async fn start<T, E, A>(name: &str, transport: T) -> Result<Server, String>
where
    T: IntoTransport<RoleClient, E, A>,
    E: std::error::Error + Send + Sync + 'static,
{
    let info = ClientConfig::new(
        ClientCapabilities::default(),
        Implementation::new("kern", "1"),
    );
    let client = tokio::time::timeout(CONNECT_TIMEOUT, info.serve(transport))
        .await
        .map_err(|_| timed_out(name, "connect", CONNECT_TIMEOUT))?
        .map_err(|e| e.to_string())?;
    let listed = tokio::time::timeout(CONNECT_TIMEOUT, client.peer().list_all_tools()).await;
    let listed = match listed {
        Ok(Ok(tools)) => tools,
        Ok(Err(e)) => {
            close_quietly(client).await;
            return Err(e.to_string());
        }
        Err(_) => {
            close_quietly(client).await;
            return Err(timed_out(name, "listTools", CONNECT_TIMEOUT));
        }
    };
    let peer = client.peer().clone();
    let tools = listed
        .into_iter()
        .map(|t| Arc::new(McpTool::new(name, t, peer.clone(), CALL_TIMEOUT)) as Arc<dyn Tool>)
        .collect();
    Ok(Server { client, tools })
}

async fn start_config(cfg: &McpServerConfig, handed: &ChildEnv) -> Result<Server, String> {
    match &cfg.transport {
        McpTransport::Stdio {
            command,
            args,
            env,
            cwd,
        } => {
            let child = transport::stdio(command, args, handed, env, cwd.as_ref())?;
            start(&cfg.name, child).await
        }
        McpTransport::Http { url, headers } => {
            start(&cfg.name, transport::http(url, headers)?).await
        }
    }
}

impl McpHub {
    /// A hub whose stdio servers get their own `env` and nothing else.
    pub fn new() -> Self {
        Self::default()
    }

    /// A hub whose stdio servers get `env`, with each one's own `env` over it.
    pub fn with_env(env: ChildEnv) -> Self {
        Self {
            env,
            ..Self::default()
        }
    }

    /// Connect every server at once. One that fails is logged and recorded in
    /// [`failures`](Self::failures), never returned as an error. Tools keep the
    /// order of `configs`.
    pub async fn connect(&self, configs: &[McpServerConfig], log: &(dyn Fn(&str) + Sync)) {
        let started = join_all(configs.iter().map(|cfg| async move {
            let res = start_config(cfg, &self.env).await;
            self.settle(&cfg.name, res, log)
        }))
        .await;
        self.lock_servers().extend(started.into_iter().flatten());
    }

    /// PLUG POINT: connect a server over any rmcp transport (an in-process
    /// server, a socket the host opened), with the same naming and failure rules.
    pub async fn connect_transport<T, E, A>(
        &self,
        name: &str,
        transport: T,
        log: &(dyn Fn(&str) + Sync),
    ) where
        T: IntoTransport<RoleClient, E, A>,
        E: std::error::Error + Send + Sync + 'static,
    {
        let res = start(name, transport).await;
        if let Some(server) = self.settle(name, res, log) {
            self.lock_servers().push(server);
        }
    }

    fn settle(
        &self,
        name: &str,
        res: Result<Server, String>,
        log: &(dyn Fn(&str) + Sync),
    ) -> Option<Server> {
        match res {
            Ok(server) => {
                log(&format!("mcp {name}: {} tools", server.tools.len()));
                Some(server)
            }
            Err(error) => {
                log(&format!("mcp {name}: FAILED {error}"));
                let failure = McpFailure {
                    name: name.to_string(),
                    error,
                };
                self.failures.lock().unwrap().push(failure);
                None
            }
        }
    }

    fn lock_servers(&self) -> std::sync::MutexGuard<'_, Vec<Server>> {
        self.servers.lock().unwrap()
    }

    /// Every connected server's tools.
    pub fn tools(&self) -> Vec<Arc<dyn Tool>> {
        self.lock_servers()
            .iter()
            .flat_map(|s| s.tools.iter().cloned())
            .collect()
    }

    /// The servers that did not connect, in the order they failed.
    pub fn failures(&self) -> Vec<McpFailure> {
        self.failures.lock().unwrap().clone()
    }

    /// Shut every server down (a stdio server's process included) and forget
    /// them; their tools fail from now on. Dropping the hub does the same
    /// without waiting.
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
