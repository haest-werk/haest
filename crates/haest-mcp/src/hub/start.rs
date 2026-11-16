//! Starting one server: reach it (stdio, HTTP, or a host's own transport),
//! `initialize`, list its tools, and name them apart from the tools already held.

use std::{collections::HashSet, sync::Arc};

use haest_core::{ChildEnv, tool::Tool};
use rmcp::{
    RoleClient, ServiceExt,
    model::{ClientCapabilities, ClientConfig, Implementation},
    transport::IntoTransport,
};

use super::{CLOSE_TIMEOUT, CONNECT_TIMEOUT, Client, McpStartError, Server};
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

pub(super) async fn close_quietly(mut client: Client) {
    let _ = client.close_with_timeout(CLOSE_TIMEOUT).await;
}

/// Initialize, then list the tools; a failure closes what was opened.
pub(super) async fn start<T, E, A>(transport: T) -> Result<Started, McpStartError>
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
        .map_err(|_| McpStartError::InitializeTimedOut(CONNECT_TIMEOUT))?
        .map_err(|e| McpStartError::Initialize(e.into()))?;
    let listed = tokio::time::timeout(CONNECT_TIMEOUT, client.peer().list_all_tools()).await;
    let listed = match listed {
        Ok(Ok(tools)) => tools,
        Ok(Err(e)) => {
            close_quietly(client).await;
            return Err(McpStartError::ListTools(e.into()));
        }
        Err(_) => {
            close_quietly(client).await;
            return Err(McpStartError::ListToolsTimedOut(CONNECT_TIMEOUT));
        }
    };
    Ok(Started { client, listed })
}

/// Reach the server `cfg` names (a stdio one with `handed` as its base
/// environment), then [`start`] it.
pub(super) async fn start_config(
    cfg: &McpServerConfig,
    handed: &ChildEnv,
) -> Result<Started, McpStartError> {
    match &cfg.transport {
        McpTransport::Stdio(s) => {
            let child = transport::stdio(&s.command, &s.args, handed, &s.env, s.cwd.as_deref())?;
            start(child).await
        }
        McpTransport::Http(h) => start(transport::http(&h.url, &h.headers)?).await,
    }
}
