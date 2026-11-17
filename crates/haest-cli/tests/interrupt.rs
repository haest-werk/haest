//! Ctrl+C on the `haest` binary while no turn runs.

#![cfg(unix)]

mod support;

use std::{
    io::{BufRead, BufReader},
    process::Stdio,
    sync::mpsc,
    time::Duration,
};

use support::Home;

/// A mod's slash command whose promise never settles: Ctrl+C gives it up, and
/// print mode exits with the shell's 130 rather than waiting for good.
#[test]
fn ctrl_c_gives_up_a_slash_command_that_never_answers() {
    let home = Home::new("interrupt-command");
    let mods = home.0.join(".config/haest/mods");
    std::fs::create_dir_all(&mods).unwrap();
    std::fs::write(
        mods.join("wait.js"),
        "export default function (haest) {\n  haest.command('wait', () => {\n    haest.notice('waiting')\n    return new Promise(() => {})\n  })\n}\n",
    )
    .unwrap();
    let mut child = home
        .command(&["--mods", "-p", "/wait"])
        .env("OPENROUTER_API_KEY", "k")
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();

    // The command runs once its notice is out.
    let (said, lines) = mpsc::channel();
    let stderr = BufReader::new(child.stderr.take().unwrap());
    std::thread::spawn(move || {
        for line in stderr.lines().map_while(Result::ok) {
            let _ = said.send(line);
        }
    });
    let mut seen = Vec::new();
    while !seen.iter().any(|l: &String| l.contains("waiting")) {
        match lines.recv_timeout(Duration::from_secs(30)) {
            Ok(line) => seen.push(line),
            Err(_) => {
                let _ = child.kill();
                panic!("the command never ran: {seen:?}");
            }
        }
    }

    let interrupted = std::process::Command::new("kill")
        .args(["-INT", &child.id().to_string()])
        .status()
        .unwrap();
    assert!(interrupted.success());
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if std::time::Instant::now() > deadline {
            let _ = child.kill();
            panic!("still running 10 s after Ctrl+C: {seen:?}");
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    assert_eq!(status.code(), Some(130), "{status:?}: {seen:?}");
}
