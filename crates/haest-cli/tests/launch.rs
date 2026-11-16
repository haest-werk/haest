//! The `haest` binary as a human launches it: what a run that cannot start
//! leaves behind, and what an odd environment does to a launch.

use std::{
    path::PathBuf,
    process::{Command, Output, Stdio},
};

/// A home of its own (haest's files and data under it) with a project dir in
/// it; gone at the end.
struct Home(PathBuf);

impl Home {
    fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("haest-cli-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("w")).unwrap();
        Home(dir)
    }

    /// `haest <args>` in the project, stdin empty, its environment only HOME
    /// and what the caller adds.
    fn command(&self, args: &[&str]) -> Command {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_haest"));
        cmd.arg("-C")
            .arg(self.0.join("w"))
            .args(args)
            .env_clear()
            .env("HOME", &self.0)
            .stdin(Stdio::null());
        cmd
    }

    /// [`Self::command`] with `vars`, run to its end.
    fn haest(&self, args: &[&str], vars: &[(&str, &str)]) -> Output {
        let mut cmd = self.command(args);
        cmd.envs(vars.iter().copied()).output().unwrap()
    }

    /// The session files haest saved.
    fn sessions(&self) -> Vec<PathBuf> {
        match std::fs::read_dir(self.0.join(".local/share/haest/sessions")) {
            Ok(dir) => dir.map(|e| e.unwrap().path()).collect(),
            Err(_) => Vec::new(),
        }
    }
}

impl Drop for Home {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

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
