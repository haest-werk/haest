//! Ported from tools/web-fetch.test.ts (net guard).

use std::{
    net::IpAddr,
    sync::atomic::{AtomicUsize, Ordering},
};

use async_trait::async_trait;
use haest_http::BoxError;
use url::Url;

use super::{Resolve, UrlRefused, assert_public_url, is_blocked};

const PUBLIC: &str = "93.184.215.14";

#[test]
fn private_loopback_link_local_cgnat_and_mapped_addresses_are_blocked_public_ones_pass() {
    let blocked = |ip: &str| is_blocked(ip.parse().unwrap());
    for ip in [
        "127.0.0.1",
        "10.1.2.3",
        "172.16.0.12",
        "192.168.1.1",
        "169.254.169.254",
        "100.64.0.1",
        "::1",
        "fd00::1",
        "fe80::1",
        "::ffff:127.0.0.1",
        "0.0.0.0",
        "::127.0.0.1",      // IPv4-compatible
        "2002:7f00:1::",    // 6to4, embeds 127.0.0.1
        "::",               // unspecified
        "224.0.0.1",        // multicast
        "255.255.255.255",  // broadcast
        "198.18.0.1",       // benchmarking
        "192.0.2.1",        // documentation (TEST-NET-1)
        "198.51.100.7",     // documentation (TEST-NET-2)
        "203.0.113.255",    // documentation (TEST-NET-3)
        "64:ff9b::a14:70c", // NAT64 of 10.20.7.12
        "64:ff9b:1::a00:1", // local-use NAT64 (RFC 8215) of 10.0.0.1
        "ff02::1",          // IPv6 multicast
    ] {
        assert!(blocked(ip), "{ip} must be blocked");
    }
    for ip in [
        PUBLIC,
        "1.1.1.1",
        "2606:4700:4700::1111",
        "172.32.0.1",
        "100.128.0.1",
        "::ffff:1.1.1.1",
        "192.0.3.1",
        "198.51.101.1",
        "203.0.114.1",
    ] {
        assert!(!blocked(ip), "{ip} is public");
    }
}

#[test]
fn ipv6_passes_only_in_global_unicast_outside_the_ranges_the_internet_does_not_reach() {
    let blocked = |ip: &str| is_blocked(ip.parse().unwrap());
    for ip in [
        "fec0::1",          // site-local (deprecated, still routed inside sites)
        "::ffff:0:a00:1",   // IPv4-translated 10.0.0.1
        "100::1",           // discard-only
        "5f00::1",          // segment routing SIDs
        "4000::1",          // unassigned, outside 2000::/3
        "2001::1",          // Teredo, in the IETF protocol assignments (2001::/23)
        "2001:2::1",        // benchmarking, same block
        "2001:1ff:ffff::1", // the last of that block
        "2001:db8::1",      // documentation
        "2002:a00:1::1",    // 6to4 of 10.0.0.1
        "3fff::1",          // documentation (RFC 9637)
        "3fff:fff:ffff::1", // the last of it
        "64:ff9b::808:808", // NAT64, even of a public IPv4 address
    ] {
        assert!(blocked(ip), "{ip} must be blocked");
    }
    for ip in [
        "2001:200::1",          // just past the IETF block
        "2001:4860:4860::8888", // a public resolver
        "2001:db9::1",          // just past documentation
        "2003::1",              // just past 6to4
        "3fff:1000::1",         // just past documentation (RFC 9637)
        "2a00:1450:4001::1",
        "2600::1",
    ] {
        assert!(!blocked(ip), "{ip} is public");
    }
}

/// Answers every name with fixed addresses, counting the lookups.
struct Fixed(Vec<&'static str>, AtomicUsize);

impl Fixed {
    fn new(ips: &[&'static str]) -> Self {
        Self(ips.to_vec(), AtomicUsize::new(0))
    }
    fn lookups(&self) -> usize {
        self.1.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl Resolve for Fixed {
    async fn resolve(&self, _: &str) -> Result<Vec<IpAddr>, BoxError> {
        self.1.fetch_add(1, Ordering::SeqCst);
        Ok(self.0.iter().map(|s| s.parse().unwrap()).collect())
    }
}

fn url(s: &str) -> Url {
    Url::parse(s).unwrap()
}

#[tokio::test]
async fn a_host_is_refused_when_any_address_it_resolves_to_is_private() {
    let mixed = Fixed::new(&[PUBLIC, "10.0.0.5"]);
    let refused = assert_public_url(&url("https://mixed.test/"), &mixed).await;
    let Err(UrlRefused::Blocked { ip, .. }) = refused else {
        panic!("not a Blocked refusal: {refused:?}");
    };
    assert_eq!(ip, "10.0.0.5".parse::<IpAddr>().unwrap());
    let fine = Fixed::new(&[PUBLIC]);
    let ok = assert_public_url(&url("https://fine.test/"), &fine)
        .await
        .unwrap();
    assert_eq!(
        ok,
        [PUBLIC.parse::<IpAddr>().unwrap()],
        "the checked addresses"
    );
}

#[tokio::test]
async fn ip_literals_odd_ipv4_spellings_and_non_http_schemes_are_refused_without_a_lookup() {
    let never = Fixed::new(&[PUBLIC]);
    for u in [
        "http://127.0.0.1:9999/",
        "http://2130706433/",
        "http://0x7f.1/",
        "http://[::1]/",
        "file:///etc/passwd",
        "ftp://files.test/x",
    ] {
        assert!(
            assert_public_url(&url(u), &never).await.is_err(),
            "{u} must be refused"
        );
    }
    assert_eq!(never.lookups(), 0);
}

#[tokio::test]
async fn credentials_unresolvable_and_addressless_hosts_are_refused() {
    let any = Fixed::new(&[PUBLIC]);
    let creds = assert_public_url(&url("https://u:p@site.test/"), &any).await;
    assert!(matches!(creds, Err(UrlRefused::Credentials)));
    assert_eq!(any.lookups(), 0);
    let empty = Fixed::new(&[]);
    let void = assert_public_url(&url("https://void.test/"), &empty).await;
    assert!(matches!(void, Err(UrlRefused::NoAddress { .. })));
    struct Failing;
    #[async_trait]
    impl Resolve for Failing {
        async fn resolve(&self, _: &str) -> Result<Vec<IpAddr>, BoxError> {
            Err("NXDOMAIN".into())
        }
    }
    let gone = assert_public_url(&url("https://gone.test/"), &Failing).await;
    let Err(UrlRefused::Unresolved { source, .. }) = gone else {
        panic!("not an Unresolved refusal: {gone:?}");
    };
    assert_eq!(source.to_string(), "NXDOMAIN", "the resolver's own reason");
}
