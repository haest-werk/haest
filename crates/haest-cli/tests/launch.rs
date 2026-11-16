//! The `haest` binary as a human launches it: what a run that cannot start
//! leaves behind, and what an odd environment does to a launch.

mod support;

use std::path::PathBuf;

use support::{Home, stderr};

/// A run that fails before its first turn leaves no new, empty session for
/// `haest -r` to resume instead of the real previous one.
#[test]
fn a_run_that_cannot_start_leaves_no_session_behind() {
    let home = Home::new("cannot-start");
    let no_key = home.haest(&["-p", "hi"], &[]);
    assert_eq!(no_key.status.code(), Some(2), "{}", stderr(&no_key));
    assert_eq!(home.sessions(), Vec::<PathBuf>::new(), "no key");

    let nothing_asked = home.haest(&["-p"], &[("OPENROUTER_API_KEY", "k")]);
    assert_eq!(
        nothing_asked.status.code(),
        Some(2),
        "{}",
        stderr(&nothing_asked)
    );
    assert_eq!(home.sessions(), Vec::<PathBuf>::new(), "nothing to ask");
}

/// A variable that is not Unicode, in its name or its value, is skipped: the
/// launch goes on, and the other variables still count.
#[cfg(unix)]
#[test]
fn a_variable_that_is_not_unicode_is_skipped_never_a_crash() {
    use std::{ffi::OsStr, os::unix::ffi::OsStrExt};

    let home = Home::new("not-unicode");
    let garbage = OsStr::from_bytes(b"\xff\xfe");
    let out = home
        .command(&["settings"])
        .env(OsStr::from_bytes(b"BAD_\xff"), "x")
        .env("HAEST_EFFORT", garbage)
        .env("HAEST_MODEL", "a:b")
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    let shown = String::from_utf8_lossy(&out.stdout);
    let model = shown.lines().find(|l| l.starts_with("model ")).unwrap();
    assert!(
        model.contains("a:b") && model.contains("HAEST_MODEL"),
        "{model}"
    );
}
