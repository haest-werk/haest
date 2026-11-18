//! One line from the human while a turn runs: the answer to an approval ask.
//!
//! Only what is typed once the question is on the screen answers it. The
//! prompter reads only after the terminal has drawn the question, and
//! [`read_answer`] first drops what was typed and not read yet: a line typed
//! at a question the model's own text drew, or a key left at an ask given up.
//! On a terminal the line is then read from it a byte at a time, so nothing
//! past the line is taken, and no buffer between the terminal and haest (std's
//! `Stdin` keeps one) can hold a byte typed before.
//!
//! The read is WITHDRAWABLE. An ask the gate gives up on (an interrupt, the
//! timeout) must not leave a reader blocked on stdin, or it would swallow the
//! next line typed at the REPL prompt.

#[cfg(unix)]
use std::os::fd::RawFd;
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

/// The answer to the question just drawn: a line typed from now on. None
/// when stdin closed or the read was withdrawn.
pub(crate) async fn read_answer() -> Option<String> {
    drop_typed_ahead();
    let stop = Arc::new(AtomicBool::new(false));
    let _withdraw = Withdraw(stop.clone());
    tokio::task::spawn_blocking(move || read_line_until(&stop))
        .await
        .ok()
        .flatten()
}

/// A line from stdin through std's buffer: for a pipe or a file (a parent
/// or a test writing answers), where one read can hold many lines.
fn read_one() -> Option<String> {
    let mut line = String::new();
    match std::io::stdin().lock().read_line(&mut line) {
        Ok(0) | Err(_) => None,
        Ok(_) => Some(line),
    }
}

#[cfg(unix)]
fn read_line_until(stop: &AtomicBool) -> Option<String> {
    if std::io::stdin().is_terminal() {
        terminal_line(libc::STDIN_FILENO, stop)
    } else if readable(libc::STDIN_FILENO, stop) {
        read_one()
    } else {
        None
    }
}

#[cfg(not(unix))]
fn read_line_until(_stop: &AtomicBool) -> Option<String> {
    read_one()
}

/// Waits in 100 ms slices until `fd` has something to read, so a withdrawn
/// read lets go of stdin before the REPL reads it again. False once `stop`
/// is raised, or when the wait fails.
#[cfg(unix)]
fn readable(fd: RawFd, stop: &AtomicBool) -> bool {
    let mut pollfd = libc::pollfd {
        fd,
        events: libc::POLLIN,
        revents: 0,
    };
    while !stop.load(Ordering::Relaxed) {
        #[expect(unsafe_code, reason = "poll(2) waits on stdin without taking it")]
        // SAFETY: one valid, initialised pollfd, and the count says one.
        let ready = unsafe { libc::poll(&raw mut pollfd, 1, 100) };
        if ready > 0 {
            return true;
        }
        let interrupted = std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted;
        if ready < 0 && !interrupted {
            return false;
        }
    }
    false
}

/// A line from the terminal `fd`, a byte at a time: what follows the line
/// stays in the terminal, where [`drop_typed_ahead`] reaches it, and a
/// withdrawn read lets go between any two bytes (a key typed, then Ctrl+D,
/// hands over half a line). None when the terminal closed (Ctrl+D on an
/// empty line), the read failed or it was withdrawn.
#[cfg(unix)]
fn terminal_line(fd: RawFd, stop: &AtomicBool) -> Option<String> {
    let mut line = Vec::new();
    while line.last() != Some(&b'\n') {
        if !readable(fd, stop) {
            return None;
        }
        let mut byte = 0_u8;
        #[expect(
            unsafe_code,
            reason = "read(2) of one byte; std's Stdin would buffer what follows"
        )]
        // SAFETY: a valid one-byte buffer, and the count says one.
        let n = unsafe { libc::read(fd, (&raw mut byte).cast(), 1) };
        match n {
            1 => line.push(byte),
            // Ctrl+D: on an empty line the terminal closed; after a key or
            // two it is the end of a line with no newline.
            0 if line.is_empty() => return None,
            0 => break,
            _ => {
                let kind = std::io::Error::last_os_error().kind();
                if !matches!(
                    kind,
                    std::io::ErrorKind::Interrupted | std::io::ErrorKind::WouldBlock
                ) {
                    return None;
                }
            }
        }
    }
    Some(String::from_utf8_lossy(&line).into_owned())
}

/// Drops what was typed at the terminal and not read yet, a whole line or
/// keys with no Enter: only what is typed from now on answers. Stdin that
/// is not a terminal (a pipe a parent or a test writes answers into) keeps
/// what is in it.
#[cfg(unix)]
fn drop_typed_ahead() {
    if std::io::stdin().is_terminal() {
        drop_typed_ahead_on(libc::STDIN_FILENO);
    }
}

#[cfg(unix)]
fn drop_typed_ahead_on(fd: RawFd) {
    #[expect(unsafe_code, reason = "tcflush(3) drops a terminal's unread input")]
    // SAFETY: no memory is passed; a descriptor that is not a terminal is an
    // error, and nothing is dropped then.
    unsafe {
        libc::tcflush(fd, libc::TCIFLUSH);
    }
}

/// Drops the console's unread input (keys typed, Enter or not) when stdin
/// is a console. An msys / mintty terminal is a pipe to haest, and keeps
/// what is in it.
#[cfg(windows)]
#[expect(
    unsafe_code,
    reason = "FlushConsoleInputBuffer drops a console's unread input; std cannot"
)]
fn drop_typed_ahead() {
    use std::os::windows::io::AsRawHandle;

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn FlushConsoleInputBuffer(console_input: *mut std::ffi::c_void) -> i32;
    }
    let stdin = std::io::stdin();
    if stdin.is_terminal() {
        // SAFETY: stdin's handle, valid for the process's life; a handle
        // that is not a console is an error, and nothing is dropped then.
        unsafe {
            FlushConsoleInputBuffer(stdin.as_raw_handle());
        }
    }
}

#[cfg(not(any(unix, windows)))]
fn drop_typed_ahead() {}

#[cfg(all(test, unix))]
mod tests;
