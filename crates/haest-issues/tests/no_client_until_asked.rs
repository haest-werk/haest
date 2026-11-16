//! A GitHub tracker builds no HTTP client when it is made: a host that hands in
//! its own with `with_http` never pays for (or risks) the default one.
//!
//! Its own test binary: it reads the process-wide TLS provider, which any
//! test that sends a request installs.

use haest_issues::GithubTracker;

#[test]
fn making_a_tracker_builds_no_client_and_sets_up_no_tls() {
    let tracker = GithubTracker::new("owner", "repo").with_token("a token");
    assert!(
        rustls::crypto::CryptoProvider::get_default().is_none(),
        "making the tracker set up TLS for a client nobody asked for yet"
    );
    drop(tracker);
}
