//! WebFetch's own HTTP client: haest's reqwest client
//! ([`ReqwestClient::configured`]) that follows no redirect (WebFetch follows
//! them by hand so the net guard sees every hop) and resolves names through
//! the guard's [`Resolve`] when it connects, refusing a name with any blocked
//! address.
//!
//! For a name, what it connects to is what was checked, at the moment of
//! connecting: a name that rebinds to a private address after the guard's
//! first look is refused, not reached. An IP-literal URL never reaches a
//! resolver, so this client alone would connect to `http://127.0.0.1/`: the
//! guard's check before every hop ([`guarded_get`](super::get::guarded_get))
//! is what refuses a literal.

use std::{
    net::{IpAddr, SocketAddr},
    sync::Arc,
};

use haest_http::{
    ReqwestClient,
    reqwest::{
        dns::{self, Addrs, Name, Resolving},
        redirect,
    },
};

use crate::net_guard::{ResolveRef, resolve_public};

/// reqwest's resolver for WebFetch: names resolve through the guard's
/// [`Resolve`](crate::Resolve), and a name with any blocked address does not
/// resolve at all.
struct Guarded(ResolveRef);

impl dns::Resolve for Guarded {
    fn resolve(&self, name: Name) -> Resolving {
        let resolve = self.0.clone();
        Box::pin(async move {
            let addresses = resolve_public(name.as_str(), &*resolve).await?;
            Ok(socket_addrs(addresses))
        })
    }
}

/// `ips` as reqwest's resolver hands them over. The port is the URL's:
/// reqwest replaces this 0.
fn socket_addrs(ips: Vec<IpAddr>) -> Addrs {
    Box::new(ips.into_iter().map(|ip| SocketAddr::new(ip, 0)))
}

/// WebFetch's own client over `resolve`: no redirects, the guard at connect time.
pub(super) fn guarded_client(resolve: ResolveRef) -> ReqwestClient {
    client_over(Arc::new(Guarded(resolve)))
}

/// WebFetch's client settings over the resolver `dns`: no redirects.
fn client_over(dns: Arc<dyn dns::Resolve>) -> ReqwestClient {
    ReqwestClient::configured(move |builder| {
        builder
            .redirect(redirect::Policy::none())
            .dns_resolver(dns.clone())
    })
}

#[cfg(test)]
mod tests;
