//! Ported from tools/web-fetch.test.ts (net guard).

use std::{
    net::IpAddr,
    sync::atomic::{AtomicUsize, Ordering},
};

use async_trait::async_trait;
use url::Url;

use super::{Resolve, assert_public_url, is_blocked};

const PUBLIC: &str = "93.184.215.14";

#[test]
fn private_loopback_link_local_cgnat_and_mapped_addresses_are_blocked_public_ones_pass() {
    let blocked = |ip: &str| is_blocked(ip.parse().unwrap());
    for ip in [
        "127.0.0.1",
        "10.1.2.3",
        "172.20.7.12",
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
    async fn resolve(&self, _: &str) -> Result<Vec<IpAddr>, String> {
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
    assert!(
        assert_public_url(&url("https://mixed.test/"), &mixed)
            .await
            .is_err()
    );
    let fine = Fixed::new(&[PUBLIC]);
    let ok = assert_public_url(&url("https://fine.test/"), &fine)
        .await
        .unwrap();
    assert_eq!(
        ok,
        [PUBLIC.parse::<IpAddr>().unwrap()],
        "the checked addresses, for pinning"
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
    assert!(
        assert_public_url(&url("https://u:p@site.test/"), &any)
            .await
            .is_err()
    );
    assert_eq!(any.lookups(), 0);
    let empty = Fixed::new(&[]);
    assert!(
        assert_public_url(&url("https://void.test/"), &empty)
            .await
            .is_err()
    );
    struct Failing;
    #[async_trait]
    impl Resolve for Failing {
        async fn resolve(&self, _: &str) -> Result<Vec<IpAddr>, String> {
            Err("NXDOMAIN".into())
        }
    }
    assert!(
        assert_public_url(&url("https://gone.test/"), &Failing)
            .await
            .is_err()
    );
}
