//! Running turns: events to the terminal, Ctrl+C to stop one, the cost line
//! after, and the session file flushed before the next prompt. What reaches
//! main's inbox meanwhile is steered into the turn running (TS `turns.ts`);
//! what is left runs as the next turn, through the gate (`inbox.rs`).

use std::{process::ExitCode, time::Duration};

use haest::{
    AgentControl, LoopEvent, StopReason, TurnOutcome,
    runtime::{RuntimeHooks, kill_jobs, session_usd},
    tools::JobHub,
};
use tokio::{sync::oneshot, task::JoinHandle};

use crate::term::Runtime;

/// How often print mode looks again while a background subagent still works.
const SETTLE_POLL: Duration = Duration::from_millis(250);

/// How long the second Ctrl+C gives the cancelled turn to end on its own, and
/// then the session writer to land what it holds, before haest exits anyway.
const QUIT_GRACE: Duration = Duration::from_secs(1);

/// How often the second Ctrl+C looks whether the turn has ended.
const QUIT_POLL: Duration = Duration::from_millis(20);

/// The second Ctrl+C, before haest exits: the turn, cancelled at the first, gets
/// `grace` to end (`ended` resolves when it has), so a tool that heeds the stop
/// is answered and the answer queued; then the background jobs are killed and
/// the session writer's queue lands, within `grace` too. What does not make it
/// in time is left (replay repairs a call left unanswered): haest exits promptly.
async fn wind_down(
    ended: impl Future<Output = ()>,
    hooks: &RuntimeHooks,
    jobs: Option<&JobHub>,
    grace: Duration,
) {
    let _ = tokio::time::timeout(grace, ended).await;
    if let Some(jobs) = jobs {
        kill_jobs(jobs).await;
    }
    let _ = tokio::time::timeout(grace, hooks.flush()).await;
}

/// Resolves once the turn `control` steers has ended.
async fn turn_ended(control: &AgentControl) {
    while control.running() {
        tokio::time::sleep(QUIT_POLL).await;
    }
}

/// Ctrl+C stops the running turn; a second Ctrl+C in the same turn quits
/// (a tool that ignores the stop must not hold the terminal hostage), after
/// [`wind_down`]. Watches until `done` (the turn ended); a quit under way by
/// then still runs to its exit.
fn watch_interrupts(rt: &Runtime, mut done: oneshot::Receiver<()>) -> JoinHandle<()> {
    let control = rt.agent().control();
    let hooks = rt.hooks.clone();
    let jobs = rt.jobs.clone();
    tokio::spawn(async move {
        let mut asked = false;
        loop {
            tokio::select! {
                _ = &mut done => return,
                signal = tokio::signal::ctrl_c() => {
                    if signal.is_err() {
                        return;
                    }
                    control.abort();
                    if asked {
                        wind_down(turn_ended(&control), &hooks, jobs.as_deref(), QUIT_GRACE).await;
                        std::process::exit(130);
                    }
                    asked = true;
                    hooks.warn("Stopping the turn... (Ctrl+C again to quit)");
                }
            }
        }
    })
}

/// What a turn runs on.
enum Input<'a> {
    /// The human's text: through the mods, with what the gate held.
    Human(&'a str),
    /// A turn the inbox holds, already through the gate.
    Agent(&'a str),
}

/// One turn, its events on the terminal, the cost line after; Ctrl+C stops it.
async fn turn(rt: &mut Runtime, input: Input<'_>) -> TurnOutcome {
    let term = rt.ui.term.clone();
    let (done, watching) = oneshot::channel();
    let watcher = watch_interrupts(rt, watching);
    let mut usage = None;
    let mut sink = |ev: LoopEvent| {
        if let LoopEvent::TurnEnd { usage: u, .. } = &ev {
            usage = Some(u.clone());
        }
        term.lock().unwrap().event(&ev);
    };
    let end = match input {
        Input::Human(text) => rt.human_turn(text, &mut sink).await,
        Input::Agent(text) => rt.run(text, &mut sink).await,
    };
    // The watcher stops; one winding haest down for a second Ctrl+C exits instead.
    let _ = done.send(());
    let _ = watcher.await;
    if let Some(u) = usage {
        let total = session_usd(rt.agent());
        rt.ui.term.lock().unwrap().usage(&u, total);
    }
    end
}

/// A turn the human started: the mods' `prompt.submit` may rewrite the text
/// first, then anything the gate held rides in ahead of it.
pub async fn human(rt: &mut Runtime, input: &str) -> TurnOutcome {
    turn(rt, Input::Human(input)).await
}

/// Every turn the human did not start that waits, one at a time, each through the gate.
pub async fn agent_turns(rt: &mut Runtime) {
    while let Some(text) = rt.next_agent_turn() {
        turn(rt, Input::Agent(&text)).await;
    }
}

/// Print mode, after the human's turn: run what waits, and while a background
/// subagent still works, wait for its answer and run that too. Ctrl+C stops
/// the subagents and the wait.
pub async fn settle(rt: &mut Runtime) {
    loop {
        agent_turns(rt).await;
        if rt.subagents.answers_due() == 0 && rt.inbox.waiting() == 0 {
            return;
        }
        tokio::select! {
            _ = rt.inbox.wait() => {}
            _ = tokio::time::sleep(SETTLE_POLL) => {}
            _ = tokio::signal::ctrl_c() => {
                rt.hooks.warn("Stopping the subagents.");
                rt.subagents.stop_all();
                return;
            }
        }
    }
}

/// How a print-mode run exits: 0 answered, 130 interrupted, 1 anything else.
pub fn exit_code(end: &TurnOutcome) -> ExitCode {
    match end.stop_reason {
        StopReason::Aborted => ExitCode::from(130),
        StopReason::EndTurn if end.error.is_none() => ExitCode::SUCCESS,
        _ => ExitCode::FAILURE,
    }
}

#[cfg(test)]
mod tests {
    use std::{path::Path, sync::Arc, time::Instant};

    use haest::{
        Message, ToolResultMessage,
        core::fs::{Fs, MemFs},
        core::session::{JsonlSessionFile, SessionRecord},
        settings::Settings,
    };

    use super::*;
    use crate::term::TermUi;

    /// A host whose session file `/data/sessions/s.jsonl` is in `fs`.
    async fn hooks_over(fs: &Arc<MemFs>) -> Arc<RuntimeHooks> {
        let store = JsonlSessionFile::new(fs.clone(), Path::new("/data"), "s").unwrap();
        store.create("s", "/w", 0).await.unwrap();
        let ui = Arc::new(TermUi::new(false));
        Arc::new(RuntimeHooks::new(
            Settings::bare("/w", "/h"),
            ui,
            Arc::new(store),
        ))
    }

    /// A tool call's answer, as the session file records it.
    fn answer(text: &str) -> SessionRecord {
        let result = ToolResultMessage {
            tool_call_id: "c".into(),
            tool_name: "Bash".into(),
            content: text.into(),
            is_error: true,
            media: vec![],
        };
        SessionRecord::Message {
            t: 1,
            message: Message::Tool(result),
        }
    }

    /// The second Ctrl+C exits promptly, but first the cancelled turn gets a
    /// moment to answer its tool calls, and what the session writer holds lands.
    #[tokio::test]
    async fn the_second_ctrl_c_lands_the_turns_answers_before_haest_exits() {
        let fs = Arc::new(MemFs::new());
        let hooks = hooks_over(&fs).await;
        hooks.append(answer("queued before the second Ctrl+C"));
        let turn = hooks.clone();
        let ends = async move {
            tokio::time::sleep(Duration::from_millis(30)).await;
            turn.append(answer("Interrupted by the user."));
        };
        wind_down(ends, &hooks, None, Duration::from_secs(5)).await;
        let file = fs
            .read_to_string(Path::new("/data/sessions/s.jsonl"))
            .await
            .unwrap();
        assert!(file.contains("queued before the second Ctrl+C"), "{file}");
        assert!(file.contains("Interrupted by the user."), "{file}");

        let started = Instant::now();
        wind_down(
            std::future::pending(),
            &hooks,
            None,
            Duration::from_millis(50),
        )
        .await;
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "a turn that never ends holds the exit only for the grace"
        );
    }

    #[test]
    fn only_a_clean_answer_exits_zero() {
        let code = |r, e: Option<&str>| {
            let mut end = TurnOutcome::new(r);
            end.error = e.map(String::from);
            exit_code(&end)
        };
        assert_eq!(code(StopReason::EndTurn, None), ExitCode::SUCCESS);
        assert_eq!(code(StopReason::EndTurn, Some("cap")), ExitCode::FAILURE);
        assert_eq!(code(StopReason::Error, None), ExitCode::FAILURE);
        assert_eq!(code(StopReason::Aborted, None), ExitCode::from(130));
    }
}
