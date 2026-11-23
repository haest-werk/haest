//! The server counts every connection it accepts, even one that sends
//! nothing, and never its own wake-up on drop: a test that says "never
//! reached" asserts a count of 0, not only an empty log.

use std::{
    net::TcpStream,
    time::{Duration, Instant},
};

use super::Server;

/// Wait until `server` has accepted `n` connections, for at most 5 s: its
/// thread counts one a moment after the client's connect returns.
fn wait_for_connections(server: &Server, n: usize) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while server.connections() < n {
        assert!(
            Instant::now() < deadline,
            "{} connections, not {n}",
            server.connections()
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn a_connection_that_sends_nothing_counts_and_logs_no_request() {
    let server = Server::start([]);
    assert_eq!(server.connections(), 0);
    drop(TcpStream::connect(server.addr()).unwrap());
    wait_for_connections(&server, 1);
    assert_eq!(server.connections(), 1);
    assert!(server.seen().is_empty(), "{:?}", server.seen());
}

#[test]
fn dropping_the_server_does_not_count_its_own_wake_up() {
    let server = Server::start([]);
    drop(TcpStream::connect(server.addr()).unwrap());
    wait_for_connections(&server, 1);
    let count = server.connections.clone();
    // The drop connects to wake the accept loop and joins it: whatever that
    // loop counted is in when the drop returns.
    drop(server);
    assert_eq!(count.load(std::sync::atomic::Ordering::SeqCst), 1);
}

#[test]
fn a_server_never_connected_to_counts_none_through_its_drop() {
    let server = Server::start([]);
    let count = server.connections.clone();
    drop(server);
    assert_eq!(count.load(std::sync::atomic::Ordering::SeqCst), 0);
}
