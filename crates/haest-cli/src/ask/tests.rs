use std::{
    fs::File,
    io::Write,
    os::fd::{AsRawFd, FromRawFd, OwnedFd},
    ptr::null_mut,
    sync::mpsc,
    time::Duration,
};

use super::*;

/// A terminal: its keyboard, and the end haest reads.
#[expect(unsafe_code, reason = "std cannot open a pseudo-terminal")]
fn terminal() -> (File, OwnedFd) {
    let (mut master, mut slave) = (-1, -1);
    // SAFETY: two valid out-pointers; the name, termios and size may be
    // null (termios and size are `*const` on Linux, `*mut` on macOS).
    let opened = unsafe {
        libc::openpty(
            &raw mut master,
            &raw mut slave,
            null_mut(),
            null_mut(),
            null_mut(),
        )
    };
    assert_eq!(opened, 0, "openpty: {}", std::io::Error::last_os_error());
    // SAFETY: openpty made it, and nothing else owns it.
    let keyboard = unsafe { File::from_raw_fd(master) };
    // SAFETY: as above.
    let end = unsafe { OwnedFd::from_raw_fd(slave) };
    (keyboard, end)
}

/// A typed line (or Ctrl+D) waits at `fd`, up to `wait_ms` for it.
#[expect(unsafe_code, reason = "poll(2) asks whether input waits")]
fn waiting(fd: &OwnedFd, wait_ms: i32) -> bool {
    let mut pollfd = libc::pollfd {
        fd: fd.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    // SAFETY: one valid, initialised pollfd, and the count says one.
    unsafe { libc::poll(&raw mut pollfd, 1, wait_ms) > 0 }
}

/// A line typed before the question, and a key with no Enter, are dropped:
/// the answer is only what is typed after.
#[test]
fn what_was_typed_before_is_dropped_and_only_what_follows_answers() {
    let (mut keyboard, end) = terminal();
    keyboard.write_all(b"y\ny").unwrap();
    assert!(waiting(&end, 2000), "the typed line is in the terminal");
    drop_typed_ahead_on(end.as_raw_fd());
    assert!(!waiting(&end, 0), "nothing typed before is left");
    keyboard.write_all(b"\n").unwrap();
    let stop = AtomicBool::new(false);
    assert_eq!(terminal_line(end.as_raw_fd(), &stop).as_deref(), Some("\n"));
}

/// A line read from a terminal takes nothing past it: the next line stays in
/// the terminal, where a drop reaches it, not in a buffer of haest's.
#[test]
fn a_line_read_from_a_terminal_leaves_the_rest_in_it() {
    let (mut keyboard, end) = terminal();
    keyboard.write_all(b"n\ny\n").unwrap();
    let stop = AtomicBool::new(false);
    assert_eq!(
        terminal_line(end.as_raw_fd(), &stop).as_deref(),
        Some("n\n")
    );
    assert!(
        waiting(&end, 2000),
        "the next line is still in the terminal"
    );
    drop_typed_ahead_on(end.as_raw_fd());
    assert!(!waiting(&end, 0), "and is dropped there");
}

/// `terminal_line(fd)` on a thread of its own: what it read, if it is done
/// within 2 s of `stop` being raised (after 300 ms when `withdraw`).
fn read_on_a_thread(fd: RawFd, withdraw: bool) -> Result<Option<String>, mpsc::RecvTimeoutError> {
    let stop = Arc::new(AtomicBool::new(false));
    let raised = stop.clone();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || tx.send(terminal_line(fd, &raised)));
    if withdraw {
        std::thread::sleep(Duration::from_millis(300));
        stop.store(true, Ordering::Relaxed);
    }
    rx.recv_timeout(Duration::from_secs(2))
}

/// A key, then Ctrl+D, hands that key over without a newline: it is the
/// answer at once (as bash's `read` takes it). The read never waits inside
/// the terminal for the rest of a line, where it could not be withdrawn.
#[test]
fn a_key_and_ctrl_d_answer_at_once() {
    let (mut keyboard, end) = terminal();
    keyboard.write_all(b"y\x04").unwrap();
    let read = read_on_a_thread(end.as_raw_fd(), false);
    assert_eq!(read, Ok(Some("y".into())), "still waiting for the line");
}

/// A withdrawn read lets go of the terminal, a key typed or not.
#[test]
fn a_withdrawn_read_lets_go_of_the_terminal() {
    let (mut keyboard, end) = terminal();
    keyboard.write_all(b"y").unwrap();
    let read = read_on_a_thread(end.as_raw_fd(), true);
    assert_eq!(
        read,
        Ok(None),
        "the withdrawn read still holds the terminal"
    );
}

/// Ctrl+D on an empty line closes stdin: no answer.
#[test]
fn ctrl_d_on_an_empty_line_is_no_answer() {
    let (mut keyboard, end) = terminal();
    keyboard.write_all(b"\x04").unwrap();
    let stop = AtomicBool::new(false);
    assert_eq!(terminal_line(end.as_raw_fd(), &stop), None);
}
