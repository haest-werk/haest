//! WebFetch's client against a local server: it connects where the guard's
//! resolver says (never the system DNS), never to a blocked address, and
//! hands a redirect back instead of following it.

use std::{
    net::{IpAddr, Ipv4Addr},
    sync::Arc,
};

use async_trait::async_trait;
use haest_http::{
    BoxError, Request, ReqwestClient,
    reqwest::dns::{self, Name, Resolving},
    testing::{Respond, Server},
};

use super::{client_over, guarded_client, socket_addrs};
use crate::net_guard::Resolve;

/// Every name resolves to the IPv4 loopback.
struct Loopback;

#[async_trait]
impl Resolve for Loopback {
    async fn resolve(&self, _host: &str) -> Result<Vec<IpAddr>, BoxError> {
        Ok(vec![Ipv4Addr::LOCALHOST.into()])
    }
}

/// reqwest's resolver over [`Loopback`] with no guard: what lets these tests
/// reach their local server through WebFetch's client settings.
struct Unguarded;

impl dns::Resolve for Unguarded {
    fn resolve(&self, name: Name) -> Resolving {
        Box::pin(async move { Ok(socket_addrs(Loopback.resolve(name.as_str()).await?)) })
    }
}

/// WebFetch's client settings, reaching the local server.
fn local_client() -> ReqwestClient {
    client_over(Arc::new(Unguarded))
}

/// An empty `200` to every request.
fn ok() -> Server {
    Server::start([])
}

#[tokio::test]
async fn it_connects_where_the_resolver_says_never_through_the_system_dns() {
    let server = ok();
    // No DNS answers for .invalid: only the resolver's address reaches the server.
    let url = format!("http://pinned.invalid:{}/", server.addr().port());
    let res = local_client().send(Request::get(url)).await.unwrap();
    assert_eq!(res.status, 200);
    let seen = server.seen();
    let host = seen[0].header("host").unwrap_or_default();
    assert!(host.starts_with("pinned.invalid:"), "{host}");
}

#[tokio::test]
async fn a_name_that_resolves_to_a_blocked_address_when_connecting_is_never_reached() {
    let server = ok();
    let url = format!("http://rebinding.test:{}/", server.addr().port());
    let sent = guarded_client(Loopback.into())
        .send(Request::get(url))
        .await;
    assert!(sent.is_err());
    assert!(server.seen().is_empty(), "the server was reached");
    // Not even a connection that sent nothing: the guard refuses before connecting.
    assert_eq!(server.connections(), 0, "the server was connected to");
}

#[tokio::test]
async fn a_redirect_comes_back_as_the_answer_never_followed() {
    let next = "http://127.0.0.1:1/next".to_string();
    let server = Server::start([Respond::WithHeaders(
        302,
        vec![("location".into(), next)],
        String::new(),
    )]);
    let url = format!("http://site.test:{}/go", server.addr().port());
    let res = local_client().send(Request::get(url)).await.unwrap();
    assert_eq!(res.status, 302);
    assert_eq!(res.header("location"), Some("http://127.0.0.1:1/next"));
    assert_eq!(server.seen().len(), 1);
}
