//! WebFetch's default HTTP client: haest's reqwest client
//! ([`ReqwestClient::configured`]) that follows no redirect (WebFetch follows
//! them by hand so the net guard sees every hop) and resolves names through
//! the guard's [`Resolve`] when it connects, refusing a name with any blocked
//! address. What it connects to is what was checked, at the moment of
//! connecting: a name that rebinds to a private address after the guard's
//! first look is refused, not reached.

use std::{net::SocketAddr, sync::Arc};

use haest_http::{
    ReqwestClient,
    reqwest::{
        dns::{self, Addrs, Name, Resolving},
        redirect,
    },
};

use crate::net_guard::{Resolve, resolve_public};

/// reqwest's resolver for WebFetch: names resolve through `resolve`, and with
/// `guard` on (always, outside this module's tests) a name with any blocked
/// address does not resolve at all.
struct Guarded {
    resolve: Arc<dyn Resolve>,
    guard: bool,
}

impl dns::Resolve for Guarded {
    fn resolve(&self, name: Name) -> Resolving {
        let (resolve, guard) = (self.resolve.clone(), self.guard);
        Box::pin(async move {
            let addresses = if guard {
                resolve_public(name.as_str(), resolve.as_ref()).await?
            } else {
                resolve.resolve(name.as_str()).await?
            };
            // The port is the URL's: reqwest replaces this 0.
            let addrs: Addrs = Box::new(addresses.into_iter().map(|ip| SocketAddr::new(ip, 0)));
            Ok(addrs)
        })
    }
}

/// WebFetch's default client over `resolve`: no redirects, the guard at connect time.
pub(super) fn guarded_client(resolve: Arc<dyn Resolve>) -> ReqwestClient {
    client(resolve, true)
}

fn client(resolve: Arc<dyn Resolve>, guard: bool) -> ReqwestClient {
    ReqwestClient::configured(move |builder| {
        let dns = Guarded {
            resolve: resolve.clone(),
            guard,
        };
        builder.redirect(redirect::Policy::none()).dns_resolver(dns)
    })
}

#[cfg(test)]
mod tests;
