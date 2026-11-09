//! Running one turn: events to the terminal, Ctrl+C to stop it, the cost line
//! after, and the session file flushed before the next prompt.

use std::{
    process::ExitCode,
    sync::atomic::{AtomicBool, Ordering},
};

use kern::{LoopEvent, StopReason, TurnEnd};

use crate::{host::session_usd, setup::Runtime};

/// Ctrl+C stops the running turn; a second Ctrl+C in the same turn quits
/// (a tool that ignores the stop must not hold the terminal hostage).
fn watch_interrupts(rt: &Runtime) -> tokio::task::JoinHandle<()> {
    let control = rt.agent.control();
    let host = rt.host.clone();
    tokio::spawn(async move {
        let asked = AtomicBool::new(false);
        while tokio::signal::ctrl_c().await.is_ok() {
            if asked.swap(true, Ordering::Relaxed) {
                std::process::exit(130);
            }
            host.warn("Stopping the turn... (Ctrl+C again to quit)");
            control.abort();
        }
    })
}

pub async fn turn(rt: &mut Runtime, input: &str) -> TurnEnd {
    let term = rt.host.term.clone();
    let watcher = watch_interrupts(rt);
    let mut usage = None;
    let mut sink = |ev: LoopEvent| {
        if let LoopEvent::TurnEnd { usage: u, .. } = &ev {
            usage = Some(u.clone());
        }
        term.lock().unwrap().event(&ev);
    };
    let end = rt.agent.run(input, &mut sink).await;
    watcher.abort();
    if let Some(u) = usage {
        let total = session_usd(&rt.agent);
        rt.host.term.lock().unwrap().usage(&u, total);
    }
    rt.host.flush().await;
    end
}

/// A turn the human started: the mods' `prompt.submit` may rewrite the text first.
pub async fn human(rt: &mut Runtime, input: &str) -> TurnEnd {
    let text = rt.mods.rewrite_prompt(input).await;
    turn(rt, &text).await
}

/// How a print-mode run exits: 0 answered, 130 interrupted, 1 anything else.
pub fn exit_code(end: &TurnEnd) -> ExitCode {
    match end.stop_reason {
        StopReason::Aborted => ExitCode::from(130),
        StopReason::EndTurn if end.error.is_none() => ExitCode::SUCCESS,
        _ => ExitCode::FAILURE,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_clean_answer_exits_zero() {
        let code = |r, e: Option<&str>| {
            let mut end = TurnEnd::new(r);
            end.error = e.map(String::from);
            exit_code(&end)
        };
        assert_eq!(code(StopReason::EndTurn, None), ExitCode::SUCCESS);
        assert_eq!(code(StopReason::EndTurn, Some("cap")), ExitCode::FAILURE);
        assert_eq!(code(StopReason::Error, None), ExitCode::FAILURE);
        assert_eq!(code(StopReason::Aborted, None), ExitCode::from(130));
    }
}
