//! Running turns: events to the terminal, Ctrl+C to stop one, the cost line
//! after, and the session file flushed before the next prompt. What reaches
//! main's inbox meanwhile is steered into the turn running (TS `turns.ts`);
//! what is left runs as the next turn, through the gate (`inbox.rs`).

use std::{
    process::ExitCode,
    sync::atomic::{AtomicBool, Ordering},
    time::Duration,
};

use kern::{LoopEvent, StopReason, TurnEnd};

use crate::{host::session_usd, setup::Runtime, world::kill_jobs};

/// How often print mode looks again while a background subagent still works.
const SETTLE_POLL: Duration = Duration::from_millis(250);

/// Ctrl+C stops the running turn; a second Ctrl+C in the same turn quits
/// (a tool that ignores the stop must not hold the terminal hostage) -- its
/// background jobs killed first.
fn watch_interrupts(rt: &Runtime) -> tokio::task::JoinHandle<()> {
    let control = rt.agent.control();
    let host = rt.host.clone();
    let jobs = rt.jobs.clone();
    tokio::spawn(async move {
        let asked = AtomicBool::new(false);
        while tokio::signal::ctrl_c().await.is_ok() {
            if asked.swap(true, Ordering::Relaxed) {
                if let Some(jobs) = &jobs {
                    kill_jobs(jobs).await;
                }
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
    let control = rt.agent.control();
    let mut usage = None;
    let mut sink = |ev: LoopEvent| {
        if let LoopEvent::TurnEnd { usage: u, .. } = &ev {
            usage = Some(u.clone());
        }
        term.lock().unwrap().event(&ev);
    };
    let end = {
        let run = rt.agent.run(input, &mut sink);
        tokio::pin!(run);
        loop {
            tokio::select! {
                end = &mut run => break end,
                Some(text) = rt.inbox.arrival() => control.steer(text),
            }
        }
    };
    watcher.abort();
    if let Some(u) = usage {
        let total = session_usd(&rt.agent);
        rt.host.term.lock().unwrap().usage(&u, total);
    }
    rt.host.flush().await;
    end
}

/// A turn the human started: the mods' `prompt.submit` may rewrite the text
/// first, then anything the gate held rides in ahead of it.
pub async fn human(rt: &mut Runtime, input: &str) -> TurnEnd {
    let text = rt.mods.rewrite_prompt(input).await;
    let text = rt.inbox.human(&text);
    turn(rt, &text).await
}

/// Every turn the human did not start that waits, one at a time, each through the gate.
pub async fn agent_turns(rt: &mut Runtime) {
    while let Some(text) = rt.inbox.take(&rt.agent.control()) {
        if let Some(text) = rt.inbox.agent_turn(&text) {
            turn(rt, &text).await;
        }
    }
}

/// Print mode, after the human's turn: run what waits, and while a background
/// subagent still works, wait for its answer and run that too. Ctrl+C stops
/// the subagents and the wait.
pub async fn settle(rt: &mut Runtime) {
    loop {
        agent_turns(rt).await;
        if rt.agents.answers_due() == 0 && rt.inbox.waiting() == 0 {
            return;
        }
        tokio::select! {
            _ = rt.inbox.wait() => {}
            _ = tokio::time::sleep(SETTLE_POLL) => {}
            _ = tokio::signal::ctrl_c() => {
                rt.host.warn("Stopping the subagents.");
                rt.agents.stop_all();
                return;
            }
        }
    }
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
