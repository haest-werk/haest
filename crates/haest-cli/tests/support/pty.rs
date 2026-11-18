//! A pseudo-terminal to run the `haest` binary on: the test types into it as
//! a human at the keyboard would, and reads what haest shows.

use std::{
    fs::File,
    io::{Read, Write},
    os::fd::{AsRawFd, FromRawFd, OwnedFd},
    ptr::null_mut,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

/// The terminal's keyboard and screen. Its other end, for a child's stdin,
/// stdout or stderr, is what [`Pty::open`] returns with it.
pub(crate) struct Pty {
    keyboard: File,
    shown: Arc<Mutex<Vec<u8>>>,
}

impl Pty {
    /// A new terminal (canonical mode, echo on, as a shell leaves it), and
    /// the end a child runs on. Drop that end once the child has it.
    #[expect(unsafe_code, reason = "std cannot open a pseudo-terminal")]
    pub(crate) fn open() -> (Self, OwnedFd) {
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
        let master = unsafe { OwnedFd::from_raw_fd(master) };
        // SAFETY: as above.
        let slave = unsafe { OwnedFd::from_raw_fd(slave) };
        for fd in [&master, &slave] {
            // A child gets the terminal as its stdio only, not these.
            // SAFETY: a valid descriptor; no memory is passed.
            unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_SETFD, libc::FD_CLOEXEC) };
        }
        let mut screen = File::from(master.try_clone().unwrap());
        let shown = Arc::new(Mutex::new(Vec::new()));
        let sink = shown.clone();
        std::thread::spawn(move || {
            let mut buf = [0; 4096];
            // Ends when every end of the terminal is closed (EIO or 0).
            while let Ok(n @ 1..) = screen.read(&mut buf) {
                sink.lock().unwrap().extend_from_slice(&buf[..n]);
            }
        });
        let keyboard = File::from(master);
        (Self { keyboard, shown }, slave)
    }

    /// Keys typed at the terminal (`\n` is Enter).
    pub(crate) fn type_keys(&mut self, keys: &str) {
        self.keyboard.write_all(keys.as_bytes()).unwrap();
    }

    /// Everything the terminal showed so far.
    pub(crate) fn screen(&self) -> String {
        String::from_utf8_lossy(&self.shown.lock().unwrap()).into_owned()
    }

    /// Waits up to `timeout` for the screen to satisfy `done`.
    pub(crate) fn wait_for(&self, timeout: Duration, done: impl Fn(&str) -> bool) -> bool {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            if done(&self.screen()) {
                return true;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        done(&self.screen())
    }
}
