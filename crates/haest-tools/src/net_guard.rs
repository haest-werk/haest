//! THE NETWORK GUARD for every outbound request a MODEL asks for (WebFetch).
//!
//! haest runs on a machine that sits inside somebody's network: a service on
//! localhost, a NAS on the LAN, a cloud metadata endpoint at 169.254.169.254. A
//! model that is handed a URL -- or talked into one by a page it fetched -- must
//! not reach any of them. So a URL passes only when it is http(s) and EVERY
//! address its host resolves to is public; the caller re-checks each redirect hop.
//!
//! [`assert_public_url`] returns the addresses it checked, and the default HTTP
//! client connects ONLY to those (DNS pinning), so a name that answers
//! public-then-private (rebinding) cannot slip between the check and the connect.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

use async_trait::async_trait;
use url::{Host, Url};

const BLOCKED_V4: [(Ipv4Addr, u8); 11] = [
    (Ipv4Addr::new(0, 0, 0, 0), 8), // "this network"
    (Ipv4Addr::new(10, 0, 0, 0), 8),
    (Ipv4Addr::new(100, 64, 0, 0), 10), // carrier-grade NAT
    (Ipv4Addr::new(127, 0, 0, 0), 8),
    (Ipv4Addr::new(169, 254, 0, 0), 16), // link-local, cloud metadata
    (Ipv4Addr::new(172, 16, 0, 0), 12),
    (Ipv4Addr::new(192, 0, 0, 0), 24),
    (Ipv4Addr::new(192, 168, 0, 0), 16),
    (Ipv4Addr::new(198, 18, 0, 0), 15), // benchmarking
    (Ipv4Addr::new(224, 0, 0, 0), 4),   // multicast
    (Ipv4Addr::new(240, 0, 0, 0), 4),   // reserved + broadcast
];

const BLOCKED_V6: [(Ipv6Addr, u8); 7] = [
    (Ipv6Addr::new(0, 0, 0, 0, 0, 0, 0, 0), 96), // unspecified, loopback (::1), IPv4-compatible (::127.0.0.1)
    (Ipv6Addr::new(0x64, 0xff9b, 0, 0, 0, 0, 0, 0), 96), // NAT64: embeds an IPv4 address
    (Ipv6Addr::new(0x64, 0xff9b, 1, 0, 0, 0, 0, 0), 48), // local-use NAT64 (RFC 8215): embeds one too
    (Ipv6Addr::new(0x2002, 0, 0, 0, 0, 0, 0, 0), 16), // 6to4: embeds an IPv4 address (2002:7f00:1:: is 127.0.0.1)
    (Ipv6Addr::new(0xfc00, 0, 0, 0, 0, 0, 0, 0), 7),  // unique local
    (Ipv6Addr::new(0xfe80, 0, 0, 0, 0, 0, 0, 0), 10), // link-local
    (Ipv6Addr::new(0xff00, 0, 0, 0, 0, 0, 0, 0), 8),  // multicast
];

fn in_net(ip: u128, net: u128, bits: u8, width: u8) -> bool {
    let shift = u32::from(width - bits);
    (ip >> shift) == (net >> shift)
}

/// Is `ip` private, local or reserved? An IPv4-mapped IPv6 address
/// (`::ffff:127.0.0.1`) is judged as the IPv4 address it carries.
pub fn is_blocked(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => BLOCKED_V4
            .iter()
            .any(|(net, bits)| in_net(u32::from(v4).into(), u32::from(*net).into(), *bits, 32)),
        IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
            Some(v4) => is_blocked(IpAddr::V4(v4)),
            None => BLOCKED_V6
                .iter()
                .any(|(net, bits)| in_net(u128::from(v6), u128::from(*net), *bits, 128)),
        },
    }
}

/// PLUG POINT: name -> addresses. The default asks the system resolver; a test
/// (or a host with its own DNS) injects another.
#[async_trait]
pub trait Resolve: Send + Sync {
    async fn resolve(&self, host: &str) -> Result<Vec<IpAddr>, String>;
}

/// The system resolver.
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemResolve;

#[async_trait]
impl Resolve for SystemResolve {
    async fn resolve(&self, host: &str) -> Result<Vec<IpAddr>, String> {
        let addrs = tokio::net::lookup_host((host, 0))
            .await
            .map_err(|e| e.to_string())?;
        Ok(addrs.map(|a| a.ip()).collect())
    }
}

/// `host[:port]`, the way a browser's `URL.host` shows it.
pub fn host_port(url: &Url) -> String {
    let host = url.host_str().unwrap_or_default();
    match url.port() {
        Some(p) => format!("{host}:{p}"),
        None => host.to_string(),
    }
}

/// The addresses a fetch of `url` may connect to; an error, written for the
/// model, unless `url` is http(s) to a host whose EVERY address is public.
pub async fn assert_public_url(url: &Url, resolve: &dyn Resolve) -> Result<Vec<IpAddr>, String> {
    if !matches!(url.scheme(), "http" | "https") {
        return Err(format!(
            "only http and https URLs can be fetched, not {}:",
            url.scheme()
        ));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err("URLs with credentials in them are not fetched".into());
    }
    let addresses = match url.host() {
        Some(Host::Ipv4(ip)) => vec![IpAddr::V4(ip)],
        Some(Host::Ipv6(ip)) => vec![IpAddr::V6(ip)],
        Some(Host::Domain(name)) => resolve
            .resolve(name)
            .await
            .map_err(|e| format!("could not resolve {name}: {e}"))?,
        None => return Err(format!("{url} has no host")),
    };
    if addresses.is_empty() {
        return Err(format!(
            "{} resolves to no address",
            url.host_str().unwrap_or_default()
        ));
    }
    if let Some(ip) = addresses.iter().find(|ip| is_blocked(**ip)) {
        return Err(format!(
            "refusing {}: {ip} is a private, local or reserved address. haest only fetches the public internet.",
            host_port(url)
        ));
    }
    Ok(addresses)
}

#[cfg(test)]
mod tests;
