//! The `kern` binary as a human launches it: what a run that cannot start
//! leaves behind.

use std::{
    path::PathBuf,
    process::{Command, Output, Stdio},
};

/// A home of its own (kern's files and data under it) with a project dir in
/// it; gone at the end.
struct Home(PathBuf);

impl Home {
    fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("kern-cli-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("w")).unwrap();
        Home(dir)
    }

    /// `kern <args>` in the project, stdin empty, its environment only HOME
    /// and `vars`.
    fn kern(&self, args: &[&str], vars: &[(&str, &str)]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_kern"))
            .arg("-C")
            .arg(self.0.join("w"))
            .args(args)
            .env_clear()
            .env("HOME", &self.0)
            .envs(vars.iter().copied())
            .stdin(Stdio::null())
            .output()
            .unwrap()
    }

    /// The session files kern saved.
    fn sessions(&self) -> Vec<PathBuf> {
        match std::fs::read_dir(self.0.join(".local/share/kern/sessions")) {
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
/// `kern -r` to resume instead of the real previous one.
#[test]
fn a_run_that_cannot_start_leaves_no_session_behind() {
    let home = Home::new("cannot-start");
    let no_key = home.kern(&["-p", "hi"], &[]);
    assert_eq!(no_key.status.code(), Some(2), "{}", stderr(&no_key));
    assert_eq!(home.sessions(), Vec::<PathBuf>::new(), "no key");

    let nothing_asked = home.kern(&["-p"], &[("OPENROUTER_API_KEY", "k")]);
    assert_eq!(
        nothing_asked.status.code(),
        Some(2),
        "{}",
        stderr(&nothing_asked)
    );
    assert_eq!(home.sessions(), Vec::<PathBuf>::new(), "nothing to ask");
}
