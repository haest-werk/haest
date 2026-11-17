//! One line from the human while a turn runs: the answer to an approval ask.
//! The read is WITHDRAWABLE. An ask the gate gives up on (an interrupt, the
//! timeout) must not leave a reader blocked on stdin, or it would swallow the
//! next line typed at the REPL prompt.

use std::{
    io::{BufRead, IsTerminal},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

/// Can a human answer here: stdin is a terminal.
pub(crate) fn interactive() -> bool {
    std::io::stdin().is_terminal()
}

/// Raised when the read's future is dropped: the blocked reader then stops.
struct Withdraw(Arc<AtomicBool>);

impl Drop for Withdraw {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Relaxed);
    }
}

/// A line from stdin; None when stdin closed or the read was withdrawn.
pub(crate) async fn read_line() -> Option<String> {
    let stop = Arc::new(AtomicBool::new(false));
    let _withdraw = Withdraw(stop.clone());
    tokio::task::spawn_blocking(move || read_line_until(&stop))
        .await
        .ok()
        .flatten()
}

fn read_one() -> Option<String> {
    let mut line = String::new();
    match std::io::stdin().lock().read_line(&mut line) {
        Ok(0) | Err(_) => None,
        Ok(_) => Some(line),
    }
}

/// Waits in 100 ms slices, so a withdrawn read lets go of stdin before the
/// REPL reads it again. A terminal in line mode is readable only once a whole
/// line is in, so the read after the wait does not block.
#[cfg(unix)]
fn read_line_until(stop: &AtomicBool) -> Option<String> {
    let mut fd = libc::pollfd {
        fd: libc::STDIN_FILENO,
        events: libc::POLLIN,
        revents: 0,
    };
    while !stop.load(Ordering::Relaxed) {
        #[expect(unsafe_code, reason = "poll(2) waits on stdin without taking it")]
        // SAFETY: one valid, initialised pollfd, and the count says one.
        let ready = unsafe { libc::poll(&raw mut fd, 1, 100) };
        if ready > 0 {
            return read_one();
        }
        let interrupted = std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted;
        if ready < 0 && !interrupted {
            return None;
        }
    }
    None
}

#[cfg(not(unix))]
fn read_line_until(_stop: &AtomicBool) -> Option<String> {
    read_one()
}
