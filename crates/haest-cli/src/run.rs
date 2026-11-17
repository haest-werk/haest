//! Running turns: the session runs them (haest-session's driver over the
//! runtime). What reaches main's inbox while a turn runs is steered into it
//! (TS `turns.ts`); what is left runs as the next turn, through the gate
//! (`inbox.rs`); the session file is flushed after every turn. Here: Ctrl+C,
//! print mode's wait for background work, and the exit code.

use std::{process::ExitCode, sync::Arc, time::Duration};

use haest::{
    AgentControl, StopReason, TurnOutcome,
    runtime::{RuntimeHooks, kill_jobs},
    session::{Body, Frame, Input, SessionHandle, Subscription},
    tools::{JobHub, SubagentHub},
    wire::{Outbound, State},
};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::term::Runtime;

/// The conversation, running: a session over the runtime.
pub(crate) type Session = SessionHandle<Runtime>;

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

/// What Ctrl+C reaches while the session owns the runtime: taken from it
/// before it starts.
pub(crate) struct Brakes {
    control: AgentControl,
    hooks: RuntimeHooks,
    jobs: Option<JobHub>,
    subagents: SubagentHub,
}

impl Brakes {
    pub(crate) fn of(rt: &Runtime) -> Self {
        Self {
            control: rt.agent().control(),
            hooks: rt.hooks().clone(),
            jobs: rt.jobs().cloned(),
            subagents: rt.subagents().clone(),
        }
    }

    /// `text` on the terminal, as a warning.
    pub(crate) fn warn(&self, text: &str) {
        self.hooks.warn(text);
    }
}

/// Whether `frame` says a turn of the conversation's own has started.
fn turn_started(frame: &Frame) -> bool {
    frame.path.is_main()
        && matches!(&frame.body, Body::Wire(Outbound::Status(s)) if s.state == State::Running)
}

/// Ctrl+C stops the running turn; a second Ctrl+C in the same turn quits
/// (a tool that ignores the stop must not hold the terminal hostage), after
/// [`wind_down`]. Between turns it does nothing here: the prompt reads its
/// own Ctrl+C, a slash command has its own ([`unless_interrupted`]), and
/// print mode's wait has its own ([`settle`]). `stream`
/// says when a new turn starts; the watch ends with it.
#[expect(
    clippy::exit,
    reason = "a second Ctrl+C quits the CLI with the shell's 130"
)]
pub(crate) fn watch_interrupts(brakes: Arc<Brakes>, mut stream: Subscription) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut asked = false;
        loop {
            tokio::select! {
                frame = stream.recv() => match frame {
                    Some(frame) => asked &= !turn_started(&frame),
                    None => return,
                },
                signal = tokio::signal::ctrl_c() => {
                    if signal.is_err() {
                        return;
                    }
                    if !brakes.control.running() {
                        continue;
                    }
                    brakes.control.abort();
                    if asked {
                        let ended = turn_ended(&brakes.control);
                        wind_down(ended, &brakes.hooks, brakes.jobs.as_ref(), QUIT_GRACE).await;
                        std::process::exit(130);
                    }
                    asked = true;
                    brakes.hooks.warn("Stopping the turn... (Ctrl+C again to quit)");
                }
            }
        }
    })
}

/// How long a slash command stopped by Ctrl+C has to wind down (a compaction
/// books the summary call it stopped) before it is dropped.
const STOP_GRACE: Duration = Duration::from_secs(2);

/// A slash command, unless Ctrl+C comes first: then None. `cancel` fires,
/// the command gets [`STOP_GRACE`] to end (what heeds it books what it spent:
/// `/compact`'s summary call), and is dropped after that wherever it waits
/// (a session job it waits for is withdrawn, so the session goes on: a mod's
/// command that never answers). Between turns nothing else acts on Ctrl+C
/// ([`watch_interrupts`] leaves it to the prompt).
pub(crate) async fn unless_interrupted<T>(
    cancel: &CancellationToken,
    work: impl Future<Output = T>,
) -> Option<T> {
    stop_on(tokio::signal::ctrl_c(), cancel, STOP_GRACE, work).await
}

/// [`unless_interrupted`] with its interrupt and grace given.
async fn stop_on<T>(
    interrupt: impl Future<Output = std::io::Result<()>>,
    cancel: &CancellationToken,
    grace: Duration,
    work: impl Future<Output = T>,
) -> Option<T> {
    tokio::pin!(work);
    tokio::select! {
        biased;
        out = &mut work => return Some(out),
        signal = interrupt => if signal.is_err() {
            // No Ctrl+C to be had: the work's end is the only way on.
            return Some(work.await);
        },
    }
    cancel.cancel();
    // What heeds the stop ends now and books what it spent; what does not
    // is dropped after the grace.
    let _ = tokio::time::timeout(grace, work).await;
    None
}

/// A turn the human started: the mods' `prompt.submit` may rewrite the text
/// first, then anything the gate held rides in ahead of it (the session's
/// way with a person's input). Its end, or None when the session is gone.
pub(crate) async fn human(session: &Session, text: String) -> Option<TurnOutcome> {
    session.turn(Input::human(text)).await.ok()
}

/// Print mode, after the human's turn: what waits runs, and while a
/// background subagent still works, its answer is waited for and run too
/// (the session does both). Ctrl+C between turns stops the subagents and
/// the wait.
pub(crate) async fn settle(session: &Session, brakes: &Brakes) {
    let settled = session.settled();
    tokio::pin!(settled);
    loop {
        tokio::select! {
            _ = &mut settled => return,
            signal = tokio::signal::ctrl_c() => {
                if signal.is_err() {
                    return settled.await;
                }
                if brakes.control.running() {
                    // The interrupt watch stops the turn; the wait goes on.
                    continue;
                }
                brakes.hooks.warn("Stopping the subagents.");
                brakes.subagents.stop_all();
                return;
            }
        }
    }
}

/// How a print-mode run exits: 0 answered, 130 interrupted, 1 anything else.
pub(crate) fn exit_code(end: &TurnOutcome) -> ExitCode {
    match end.stop_reason {
        StopReason::Aborted => ExitCode::from(130),
        StopReason::EndTurn if end.error.is_none() => ExitCode::SUCCESS,
        _ => ExitCode::FAILURE,
    }
}

#[cfg(test)]
mod tests {
    use std::{
        path::{Path, PathBuf},
        sync::Arc,
        time::Instant,
    };

    use haest::{
        Message, ToolResultMessage,
        core::fs::{Fs, MemFs},
        core::session::SessionRecord,
        runtime::{BuildOptions, RuntimeInputs, World, build},
        session::{Hub, SessionPrompter},
        settings::Settings,
    };

    use super::*;
    use crate::term::TermUi;

    /// A conversation whose session file is in `fs`, under `/data/sessions`:
    /// its hooks, and that file.
    async fn hooks_over(fs: &Arc<MemFs>) -> (RuntimeHooks, PathBuf) {
        let settings = Settings {
            data_dir: "/data".into(),
            ..Settings::bare("/w", "/h")
        };
        let inputs = RuntimeInputs::new(settings, World::files(fs.clone()))
            .with_keys(|k: &str| (k == "OPENROUTER_API_KEY").then(|| "k".to_string()));
        let ui = TermUi::new(false);
        let asks = SessionPrompter::new(Hub::new("t"));
        let rt = build(inputs, ui, asks, BuildOptions::default())
            .await
            .unwrap();
        let file = Path::new("/data/sessions").join(format!("{}.jsonl", rt.session_id()));
        (rt.hooks().clone(), file)
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
        // Two answers the test writes: one held before the second Ctrl+C, one
        // the cancelled turn writes after it.
        const QUEUED: &str = "answer 1";
        const LATE: &str = "answer 2";
        let fs = Arc::new(MemFs::new());
        let (hooks, file) = hooks_over(&fs).await;
        hooks.append(&answer(QUEUED));
        let turn = hooks.clone();
        let ends = async move {
            tokio::time::sleep(Duration::from_millis(30)).await;
            turn.append(&answer(LATE));
        };
        wind_down(ends, &hooks, None, Duration::from_secs(5)).await;
        let file = fs.read_to_string(&file).await.unwrap();
        assert!(file.contains(QUEUED) && file.contains(LATE), "{file}");

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

    /// Ctrl+C on a command that heeds its token (`/compact`): the token fires
    /// and the command ends within the grace, so what it books on the way
    /// out (the summary call it stopped) is booked. Stopped, so None.
    #[tokio::test]
    async fn a_stopped_command_winds_down_before_it_is_dropped() {
        let cancel = CancellationToken::new();
        let booked = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (heeds, book) = (cancel.clone(), booked.clone());
        let command = async move {
            heeds.cancelled().await;
            tokio::time::sleep(Duration::from_millis(20)).await;
            book.store(true, std::sync::atomic::Ordering::SeqCst);
        };
        let out = stop_on(async { Ok(()) }, &cancel, Duration::from_secs(5), command).await;
        assert!(out.is_none(), "stopped");
        assert!(cancel.is_cancelled(), "the command was told to stop");
        assert!(
            booked.load(std::sync::atomic::Ordering::SeqCst),
            "it booked what it spent before it was dropped"
        );
    }

    /// A command that ignores its token (a mod's that never answers) is
    /// dropped once the grace is up: Ctrl+C still gets the human out.
    #[tokio::test]
    async fn a_stopped_command_that_ignores_the_stop_is_dropped_after_the_grace() {
        let started = Instant::now();
        let out = stop_on(
            async { Ok(()) },
            &CancellationToken::new(),
            Duration::from_millis(50),
            std::future::pending::<()>(),
        )
        .await;
        assert!(out.is_none());
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    /// A command that ends before any Ctrl+C is its own answer.
    #[tokio::test]
    async fn a_command_that_ends_first_answers() {
        let out = stop_on(
            std::future::pending(),
            &CancellationToken::new(),
            Duration::ZERO,
            async { 7 },
        )
        .await;
        assert_eq!(out, Some(7));
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
