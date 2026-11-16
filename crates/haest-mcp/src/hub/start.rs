//! Starting one server: reach it (stdio, HTTP, or a host's own transport),
//! `initialize`, list its tools, and name them apart from the tools already held.

use std::{collections::HashSet, sync::Arc, time::Duration};

use haest_core::{ChildEnv, tool::Tool};
use rmcp::{
    RoleClient, ServiceExt,
    model::{ClientCapabilities, ClientConfig, Implementation},
    transport::IntoTransport,
};

use super::{CLOSE_TIMEOUT, CONNECT_TIMEOUT, Client, Server};
use crate::{
    config::{McpServerConfig, McpTransport},
    tool::{McpTool, unique_tool_name},
    transport,
};

/// A server that answered, its tools listed but not yet named.
pub(super) struct Started {
    client: Client,
    pub(super) listed: Vec<rmcp::model::Tool>,
}

/// Keep `started` beside `servers`, each of its tools named apart from every
/// tool already held: two servers or tools that differ only in characters a
/// provider rejects, or past the 64th, never share a name.
pub(super) fn admit(servers: &mut Vec<Server>, name: &str, started: Started) {
    let mut taken: HashSet<String> = servers
        .iter()
        .flat_map(|s| s.tools.iter().map(|t| t.name().to_string()))
        .collect();
    let peer = started.client.peer().clone();
    let tools = started
        .listed
        .into_iter()
        .map(|t| {
            let unique = unique_tool_name(name, &t.name, &taken);
            taken.insert(unique.clone());
            Arc::new(McpTool::new(name, unique, t, peer.clone())) as Arc<dyn Tool>
        })
        .collect();
    servers.push(Server {
        client: started.client,
        tools,
    });
}

fn timed_out(name: &str, what: &str, after: Duration) -> String {
    format!("MCP {name} {what} timed out after {}ms", after.as_millis())
}

pub(super) async fn close_quietly(mut client: Client) {
    let _ = client.close_with_timeout(CLOSE_TIMEOUT).await;
}

/// Initialize, then list the tools; a failure closes what was opened.
pub(super) async fn start<T, E, A>(name: &str, transport: T) -> Result<Started, String>
where
    T: IntoTransport<RoleClient, E, A>,
    E: std::error::Error + Send + Sync + 'static,
{
    let info = ClientConfig::new(
        ClientCapabilities::default(),
        Implementation::new("haest", "1"),
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
    Ok(Started { client, listed })
}

/// Reach the server `cfg` names (a stdio one with `handed` as its base
/// environment), then [`start`] it.
pub(super) async fn start_config(
    cfg: &McpServerConfig,
    handed: &ChildEnv,
) -> Result<Started, String> {
    match &cfg.transport {
        McpTransport::Stdio {
            command,
            args,
            env,
            cwd,
        } => {
            let child = transport::stdio(command, args, handed, env, cwd.as_deref())?;
            start(&cfg.name, child).await
        }
        McpTransport::Http { url, headers } => {
            start(&cfg.name, transport::http(url, headers)?).await
        }
    }
}
