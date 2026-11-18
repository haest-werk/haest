//! The real workspace's streamed output (`shell_streaming`): straight from the
//! pipes to `on_output` as it arrives, never after the call returned.

mod common;

use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use common::{opts, real, tempdir};
use haest_core::workspace::{OutputChunks, Workspace};

#[tokio::test(flavor = "multi_thread")]
async fn streamed_output_arrives_while_the_command_runs_and_stays_out_of_the_result() {
    let (_t, dir) = tempdir();
    let seen = Arc::new(Mutex::new(String::new()));
    let into = seen.clone();
    let on_output = OutputChunks::from(move |s: &str| into.lock().unwrap().push_str(s));
    let ws = real();
    // A multi-byte character split across two writes arrives whole.
    let run = ws.shell_streaming(
        r"printf 'first \xc3'; sleep 0.05; printf '\xa9\n'; sleep 0.4; echo second >&2",
        opts(&dir),
        on_output,
    );
    tokio::pin!(run);
    let early = tokio::select! {
        r = &mut run => panic!("ended before the check: {r:?}"),
        _ = tokio::time::sleep(Duration::from_millis(250)) => seen.lock().unwrap().clone(),
    };
    assert_eq!(early, "first \u{e9}\n");
    let r = run.await;
    assert_eq!(*seen.lock().unwrap(), "first \u{e9}\nsecond\n");
    assert_eq!(
        (r.stdout.as_str(), r.stderr.as_str(), r.exit_code),
        ("", "", 0)
    );
}

/// A process that left bash's group may hold the output for good: the call's
/// pipe readers ran on after it returned, so what it printed later reached
/// `on_output` (after a job's `Done`) and was read and kept for a call that
/// had ended. Once the call has returned, nothing it prints arrives.
#[tokio::test(flavor = "multi_thread")]
async fn no_output_reaches_on_output_after_the_call_returns() {
    let (_t, dir) = tempdir();
    let mut o = opts(&dir);
    o.timeout = Duration::from_secs(1);
    let seen = Arc::new(Mutex::new(String::new()));
    let into = seen.clone();
    let on_output = OutputChunks::from(move |s: &str| into.lock().unwrap().push_str(s));
    let late = "perl -e 'use POSIX qw(setsid); setsid(); $|=1; sleep 4; print \"late\\n\"'";
    let r = real()
        .shell_streaming(&format!("{late} & echo early"), o, on_output)
        .await;
    assert!(r.timed_out);
    assert_eq!(*seen.lock().unwrap(), "early\n");
    // The escaped process prints at 4 s; the call returned at about 3 s.
    tokio::time::sleep(Duration::from_secs(3)).await;
    assert_eq!(*seen.lock().unwrap(), "early\n");
}

/// A host's own `on_output` that panicked ended the pipe's reader: the panic
/// was swallowed, the pipe closed, and the command died of SIGPIPE (141) on
/// its next write. As over the sandbox, the panic now reaches the caller,
/// once the command has run to its end; `on_output` hears nothing more.
#[tokio::test(flavor = "multi_thread")]
async fn a_panic_in_on_output_reaches_the_caller_once_the_command_ended() {
    let (_t, dir) = tempdir();
    let calls = Arc::new(AtomicUsize::new(0));
    let on_output = OutputChunks::from({
        let calls = calls.clone();
        move |s: &str| {
            calls.fetch_add(1, Ordering::SeqCst);
            panic!("a host's handler fails on {s:?}");
        }
    });
    // It writes to both pipes after the panic; the mark is left only if
    // neither write killed it.
    let script = "echo one; sleep 0.3; echo two; echo two >&2; touch ran-to-the-end; exit 3";
    let call = tokio::spawn({
        let o = opts(&dir);
        async move { real().shell_streaming(script, o, on_output).await }
    })
    .await;

    let panic = call.expect_err("the panic reaches the caller");
    let payload = panic.into_panic();
    let message = payload.downcast_ref::<String>().map(String::as_str);
    assert_eq!(message, Some("a host's handler fails on \"one\\n\""));
    assert!(
        dir.join("ran-to-the-end").exists(),
        "the command was killed by a closed pipe"
    );
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "called again after a panic"
    );
}
