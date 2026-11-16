//! THE NETWORK GUARD for every outbound request a MODEL asks for (WebFetch).
//!
//! haest runs on a machine that sits inside somebody's network: a service on
//! localhost, a NAS on the LAN, a cloud metadata endpoint at 169.254.169.254. A
//! model that is handed a URL -- or talked into one by a page it fetched -- must
//! not reach any of them. So a URL passes only when it is http(s) and EVERY
//! address its host resolves to is public; the caller re-checks each redirect hop.
//!
//! [`assert_public_url`] checks a URL before it is fetched, and WebFetch's
//! default HTTP client checks again when it connects: it resolves names
//! through the same [`Resolve`] and refuses any name with a blocked address, so
//! a name that answers public-then-private (rebinding) cannot slip between the
//! check and the connect.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

use async_trait::async_trait;
use haest_http::BoxError;
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

/// IPv6 global unicast: the only IPv6 space the guard lets through. Outside
/// it are loopback, the unspecified address, the IPv4-compatible, -mapped and
/// -translated forms, NAT64, discard-only, unique local, link-local,
/// site-local, multicast and every unassigned block.
const GLOBAL_V6: (Ipv6Addr, u8) = (Ipv6Addr::new(0x2000, 0, 0, 0, 0, 0, 0, 0), 3);

/// The parts of [`GLOBAL_V6`] the IANA IPv6 special-purpose address registry
/// marks not globally reachable, and 6to4, which embeds an IPv4 address.
const UNREACHABLE_GLOBAL_V6: [(Ipv6Addr, u8); 4] = [
    (Ipv6Addr::new(0x2001, 0, 0, 0, 0, 0, 0, 0), 23), // IETF protocol assignments: Teredo, benchmarking, ORCHID
    (Ipv6Addr::new(0x2001, 0xdb8, 0, 0, 0, 0, 0, 0), 32), // documentation
    (Ipv6Addr::new(0x2002, 0, 0, 0, 0, 0, 0, 0), 16), // 6to4: embeds an IPv4 address (2002:7f00:1:: is 127.0.0.1)
    (Ipv6Addr::new(0x3fff, 0, 0, 0, 0, 0, 0, 0), 20), // documentation (RFC 9637)
];

fn in_net(ip: u128, net: u128, bits: u8, width: u8) -> bool {
    let shift = u32::from(width - bits);
    (ip >> shift) == (net >> shift)
}

fn in_v6(ip: Ipv6Addr, (net, bits): (Ipv6Addr, u8)) -> bool {
    in_net(u128::from(ip), u128::from(net), bits, 128)
}

/// Whether `ip` is private, local or reserved.
///
/// IPv4 is a blocklist: loopback, the private ranges, link-local (cloud
/// metadata), carrier-grade NAT, multicast and the other reserved ranges.
/// IPv6 is an allowlist: only global unicast (`2000::/3`) passes, less the
/// blocks in it the IANA special-purpose registry marks not globally
/// reachable (`2001::/23` with Teredo, documentation `2001:db8::/32` and
/// `3fff::/20`) and 6to4 (`2002::/16`), which embeds an IPv4 address. An
/// IPv4-mapped IPv6 address (`::ffff:127.0.0.1`) is judged as the IPv4 address
/// it carries.
///
/// ```
/// use haest_tools::net_guard::is_blocked;
///
/// assert!(is_blocked("169.254.169.254".parse().unwrap()));
/// assert!(is_blocked("::ffff:10.0.0.1".parse().unwrap()));
/// assert!(is_blocked("fec0::1".parse().unwrap()));
/// assert!(!is_blocked("93.184.215.14".parse().unwrap()));
/// assert!(!is_blocked("2606:4700:4700::1111".parse().unwrap()));
/// ```
pub fn is_blocked(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => BLOCKED_V4
            .iter()
            .any(|(net, bits)| in_net(u32::from(v4).into(), u32::from(*net).into(), *bits, 32)),
        IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
            Some(v4) => is_blocked(IpAddr::V4(v4)),
            None => {
                !in_v6(v6, GLOBAL_V6) || UNREACHABLE_GLOBAL_V6.iter().any(|&net| in_v6(v6, net))
            }
        },
    }
}

/// PLUG POINT: name -> addresses. The default asks the system resolver
/// ([`SystemResolve`]); a test (or a host with its own DNS) injects another
/// through [`WebFetchNet::resolve`](crate::WebFetchNet::resolve).
#[async_trait]
pub trait Resolve: Send + Sync {
    /// Every address `host` (a DNS name, never an IP literal) resolves to, or
    /// why it does not resolve (only its text reaches the model). The guard
    /// refuses the URL unless every address is public.
    async fn resolve(&self, host: &str) -> Result<Vec<IpAddr>, BoxError>;
}

/// The system resolver (`getaddrinfo`, through tokio).
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemResolve;

#[async_trait]
impl Resolve for SystemResolve {
    async fn resolve(&self, host: &str) -> Result<Vec<IpAddr>, BoxError> {
        let addrs = tokio::net::lookup_host((host, 0)).await?;
        Ok(addrs.map(|a| a.ip()).collect())
    }
}

/// Why the net guard refuses a URL ([`assert_public_url`]). Its text is
/// written for the model; [`Unresolved`](Self::Unresolved) carries the
/// resolver's own error as its source.
#[derive(Debug, thiserror::Error)]
pub enum UrlRefused {
    /// Not http or https (`file:`, `ftp:`, ...).
    #[error("only http and https URLs can be fetched, not {scheme}:")]
    NotHttp {
        /// The URL's scheme.
        scheme: String,
    },
    /// A user name or password in the URL.
    #[error("URLs with credentials in them are not fetched")]
    Credentials,
    /// No host to fetch from.
    #[error("{url} has no host")]
    NoHost {
        /// The URL, as given.
        url: String,
    },
    /// The name did not resolve.
    #[error("could not resolve {host}")]
    Unresolved {
        /// The name.
        host: String,
        /// The resolver's reason.
        #[source]
        source: BoxError,
    },
    /// The name resolved to no address at all.
    #[error("{host} resolves to no address")]
    NoAddress {
        /// The name.
        host: String,
    },
    /// An address is private, local or reserved ([`is_blocked`]).
    #[error(
        "refusing {host}: {ip} is a private, local or reserved address. haest only fetches the public internet."
    )]
    Blocked {
        /// The host, as the URL names it (`host[:port]`).
        host: String,
        /// The first address [`is_blocked`] refuses.
        ip: IpAddr,
    },
}

/// `url`'s `host[:port]`, the way a browser's `URL.host` shows it (no port
/// when it is the scheme's default).
pub(crate) fn host_port(url: &Url) -> String {
    let host = url.host_str().unwrap_or_default();
    match url.port() {
        Some(p) => format!("{host}:{p}"),
        None => host.to_string(),
    }
}

/// The addresses a fetch of `url` may connect to: every address its host
/// resolves to, all of them public. A client that resolves the name again
/// must check again when it connects (WebFetch's default does).
///
/// Check every redirect hop again: a public page may redirect to a private
/// one.
///
/// # Errors
///
/// [`UrlRefused`] when `url` is not http(s), carries a user name or password,
/// has no host, does not resolve (or to no address), or resolves to any
/// address [`is_blocked`] refuses.
///
/// # Examples
///
/// ```
/// use std::net::IpAddr;
///
/// use async_trait::async_trait;
/// use haest_tools::{
///     http::BoxError,
///     net_guard::{Resolve, UrlRefused, assert_public_url},
/// };
/// use url::Url;
///
/// /// A name that answers one public and one private address.
/// struct Rebinding;
///
/// #[async_trait]
/// impl Resolve for Rebinding {
///     async fn resolve(&self, _: &str) -> Result<Vec<IpAddr>, BoxError> {
///         Ok(vec!["93.184.215.14".parse().unwrap(), "10.0.0.5".parse().unwrap()])
///     }
/// }
///
/// # #[tokio::main]
/// # async fn main() {
/// let public = Url::parse("https://93.184.215.14/page").unwrap();
/// assert_eq!(assert_public_url(&public, &Rebinding).await.unwrap().len(), 1);
/// let sneaky = Url::parse("https://example.test/").unwrap();
/// let refused = assert_public_url(&sneaky, &Rebinding).await.unwrap_err();
/// assert!(matches!(refused, UrlRefused::Blocked { .. }));
/// let file = Url::parse("file:///etc/passwd").unwrap();
/// assert!(assert_public_url(&file, &Rebinding).await.is_err());
/// # }
/// ```
pub async fn assert_public_url(
    url: &Url,
    resolve: &dyn Resolve,
) -> Result<Vec<IpAddr>, UrlRefused> {
    if !matches!(url.scheme(), "http" | "https") {
        let scheme = url.scheme().to_string();
        return Err(UrlRefused::NotHttp { scheme });
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(UrlRefused::Credentials);
    }
    let addresses = match url.host() {
        Some(Host::Ipv4(ip)) => vec![IpAddr::V4(ip)],
        Some(Host::Ipv6(ip)) => vec![IpAddr::V6(ip)],
        Some(Host::Domain(name)) => return resolve_public(name, resolve).await,
        None => {
            return Err(UrlRefused::NoHost {
                url: url.to_string(),
            });
        }
    };
    all_public(&host_port(url), addresses)
}

/// Every address `name` resolves to, all of them public: what WebFetch's
/// client connects to (the check again, at connect time).
pub(crate) async fn resolve_public(
    name: &str,
    resolve: &dyn Resolve,
) -> Result<Vec<IpAddr>, UrlRefused> {
    let addresses = (resolve.resolve(name).await).map_err(|source| UrlRefused::Unresolved {
        host: name.to_string(),
        source,
    })?;
    all_public(name, addresses)
}

/// `addresses` when there is one and every one is public; else why not,
/// naming the host as `shown`.
fn all_public(shown: &str, addresses: Vec<IpAddr>) -> Result<Vec<IpAddr>, UrlRefused> {
    let host = || shown.to_string();
    if addresses.is_empty() {
        return Err(UrlRefused::NoAddress { host: host() });
    }
    if let Some(&ip) = addresses.iter().find(|ip| is_blocked(**ip)) {
        return Err(UrlRefused::Blocked { host: host(), ip });
    }
    Ok(addresses)
}

#[cfg(test)]
mod tests;
