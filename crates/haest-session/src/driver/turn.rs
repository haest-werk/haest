//! One turn under the driver: its events to the hub, input that arrives
//! steered into it, an interrupt and a snapshot at once, every other request
//! after it. Requests are read whenever it waits on host code (the
//! rewriter), so an interrupt or a close always reaches it.

use std::{
    collections::VecDeque,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};

use futures::{StreamExt, stream::FuturesOrdered};
use haest_core::{AgentControl, LoopEvent, StopReason, TurnOutcome};
use haest_wire::{Origin, State};

use super::{Command, Driver, Leftover, TurnInput, Waiter, queued, snapshot};
use crate::{BoxFuture, Control, Driven, Input, RewritePromptRef};

/// Input steered into the running turn: it landed if a user message entered
/// history after it was steered.
struct Steer {
    /// User messages the turn had emitted when it was steered.
    at: usize,
    human: bool,
    done: Option<Waiter>,
}

/// How a turn ran: its outcome, the input steered into it, and the user
/// messages it emitted.
struct Ran {
    outcome: TurnOutcome,
    steered: Vec<Steer>,
    users: usize,
}

/// Input on its way into the running turn: a person's through the rewriter
/// first. They go in in the order they came.
#[derive(Default)]
struct Steering {
    /// Each input as the turn should read it.
    inputs: FuturesOrdered<BoxFuture<'static, Input>>,
    /// Who waits for each, in the same order.
    waiters: VecDeque<Option<Waiter>>,
}

impl Steering {
    /// `input` on its way in: a person's through `rewriter`.
    fn push(&mut self, rewriter: Option<RewritePromptRef>, mut input: Input, done: Option<Waiter>) {
        self.inputs.push_back(Box::pin(async move {
            if let (Some(rewriter), Origin::Human) = (rewriter, input.origin) {
                input.text = rewriter.rewrite_prompt(&input.text).await;
            }
            input
        }));
        self.waiters.push_back(done);
    }

    /// The next input ready to go in, and who waits for it. Cancel-safe:
    /// dropped at its await, it took nothing.
    async fn next(&mut self) -> Option<(Input, Option<Waiter>)> {
        let input = self.inputs.next().await?;
        Some((input, self.waiters.pop_front().flatten()))
    }

    fn is_empty(&self) -> bool {
        self.inputs.is_empty()
    }

    /// What is still being rewritten never goes in: who waits for it hears
    /// it ended aborted. (Dropped instead, they read Closed.)
    fn stop(self) {
        for waiter in self.waiters.into_iter().flatten() {
            let _ = waiter.send(Ok(TurnOutcome::new(StopReason::Aborted)));
        }
    }
}

/// Steer `input` in, and note when.
fn steer(control: &AgentControl, users: &AtomicUsize, input: Input, done: Option<Waiter>) -> Steer {
    control.steer(input.text);
    Steer {
        at: users.load(Ordering::Relaxed),
        human: input.origin == Origin::Human,
        done,
    }
}

impl<D: Driven> Driver<D> {
    /// Run `input` as a turn; `waiters` get its outcome.
    pub(super) async fn turn(&mut self, input: TurnInput, mut waiters: Vec<Waiter>) {
        self.publish_status(State::Running);
        let control = self.driven.control();
        let rewriter = self.driven.rewriter();
        let mut steering = Steering::default();
        let ready = match input {
            TurnInput::Human(text) => match &rewriter {
                Some(rewriter) => self.rewrite(rewriter, &text, &mut steering).await,
                None => Some(text),
            }
            .map(|text| (text, true)),
            TurnInput::HumanRewritten(text) => Some((text, true)),
            TurnInput::Ready(text) => Some((text, false)),
        };
        let ran = match ready {
            Some((text, human)) => {
                self.run_turn(&text, human, &control, rewriter, &mut steering)
                    .await
            }
            // Stopped before it ran: no model call, nothing in history.
            None => Ran {
                outcome: TurnOutcome::new(StopReason::Aborted),
                steered: Vec::new(),
                users: 0,
            },
        };
        // The turn's own input is its first user message: a steer landed only
        // if one came after it and after the steer. (A steer that arrives
        // while queued input is being turned into a message counts as landed
        // early; it lands at the boundary after the next model call, unless
        // the turn is stopped first.)
        let mut left = Leftover {
            human: false,
            waiters: Vec::new(),
            outcome: ran.outcome.clone(),
        };
        for steer in ran.steered {
            if ran.users > steer.at.max(1) {
                waiters.extend(steer.done);
            } else {
                left.human |= steer.human;
                left.waiters.extend(steer.done);
            }
        }
        self.status = self.driven.status();
        // Every frame of the turn is in the stream before anyone hears it
        // ended, so a peer's reply can follow them.
        self.publish_status(State::Idle);
        for waiter in waiters {
            let _ = waiter.send(Ok(ran.outcome.clone()));
        }
        self.drain(steering, &control, &mut left).await;
        if left.human || !left.waiters.is_empty() {
            self.leftover = Some(left);
        }
    }

    /// The turn's own input through the rewriter, while requests are read
    /// as during the turn: None when an interrupt or a close stopped it
    /// first, or every handle is gone. Input that arrives meanwhile waits
    /// for it; it is rewritten once the turn runs.
    async fn rewrite(
        &mut self,
        rewriter: &RewritePromptRef,
        text: &str,
        steering: &mut Steering,
    ) -> Option<String> {
        let mut rewrite = rewriter.rewrite_prompt(text);
        loop {
            tokio::select! {
                biased;
                text = &mut rewrite => return Some(text),
                cmd = self.rx.recv() => match cmd {
                    Some(Command::Submit { input, done }) => {
                        steering.push(Some(rewriter.clone()), input, done);
                    }
                    Some(Command::Control { control: Control::Interrupt, reply }) => {
                        let _ = reply.send(Ok(()));
                        return None;
                    }
                    Some(Command::Snapshot(tx)) => {
                        let _ = tx.send(self.snapshot(State::Running));
                    }
                    Some(cmd @ Command::Close(_)) => {
                        self.closing = true;
                        self.deferred.push_back(cmd);
                        return None;
                    }
                    Some(cmd) => self.deferred.push_back(cmd),
                    None => {
                        // Every handle is gone: nobody waits for this turn.
                        self.orphaned = true;
                        return None;
                    }
                },
            }
        }
    }

    /// The turn itself, on `text`: a person's through
    /// [`Driven::human_turn`], another program's through [`Driven::run`].
    /// Input that arrives meanwhile is steered into it, in order, once
    /// through `rewriter`.
    async fn run_turn(
        &mut self,
        text: &str,
        human: bool,
        control: &AgentControl,
        rewriter: Option<RewritePromptRef>,
        steering: &mut Steering,
    ) -> Ran {
        let users = Arc::new(AtomicUsize::new(0));
        let mut steered = Vec::new();
        let mut sink = {
            let (hub, users) = (self.hub.clone(), users.clone());
            move |ev: LoopEvent| {
                if matches!(ev, LoopEvent::User { .. }) {
                    users.fetch_add(1, Ordering::Relaxed);
                }
                hub.event(ev);
            }
        };
        let run = if human {
            self.driven.human_turn(text, &mut sink)
        } else {
            self.driven.run(text, &mut sink)
        };
        tokio::pin!(run);
        loop {
            tokio::select! {
                // The turn first: it marks itself running on its first
                // poll, so an interrupt that comes right after reaches it.
                biased;
                outcome = &mut run => {
                    let users = users.load(Ordering::Relaxed);
                    return Ran { outcome, steered, users };
                }
                Some((input, done)) = steering.next(), if !steering.is_empty() => {
                    steered.push(steer(control, &users, input, done));
                }
                cmd = self.rx.recv(), if !self.orphaned => match cmd {
                    Some(Command::Submit { input, done }) => {
                        steering.push(rewriter.clone(), input, done);
                    }
                    Some(Command::Control { control: Control::Interrupt, reply }) => {
                        control.abort();
                        let _ = reply.send(Ok(()));
                    }
                    Some(Command::Snapshot(tx)) => {
                        let queued = queued(&self.queue, &self.deferred);
                        let now = snapshot(&self.status, State::Running, queued, self.hub.last_seq());
                        let _ = tx.send(now);
                    }
                    Some(cmd @ Command::Close(_)) => {
                        control.abort();
                        self.closing = true;
                        self.deferred.push_back(cmd);
                    }
                    Some(cmd) => self.deferred.push_back(cmd),
                    None => {
                        // Every handle is gone: nobody watches this turn.
                        self.orphaned = true;
                        control.abort();
                    }
                },
            }
        }
    }

    /// Input still being rewritten when the turn ended goes in as it comes:
    /// it was not read, so it runs next. Requests are read meanwhile, as
    /// between turns: a snapshot is answered, an interrupt stops what is
    /// still being rewritten (its callers hear it ended aborted), a close or
    /// every handle gone drops it (its callers read Closed), the rest wait.
    async fn drain(&mut self, mut steering: Steering, control: &AgentControl, left: &mut Leftover) {
        while !steering.is_empty() && !self.closing && !self.orphaned {
            tokio::select! {
                biased;
                Some((input, done)) = steering.next() => {
                    control.steer(input.text);
                    left.human |= input.origin == Origin::Human;
                    left.waiters.extend(done);
                }
                cmd = self.rx.recv() => match cmd {
                    Some(Command::Control { control: Control::Interrupt, reply }) => {
                        steering.stop();
                        let _ = reply.send(Ok(()));
                        return;
                    }
                    Some(Command::Snapshot(tx)) => {
                        let _ = tx.send(self.snapshot(State::Idle));
                    }
                    Some(cmd) => {
                        self.closing |= matches!(cmd, Command::Close(_));
                        self.deferred.push_back(cmd);
                    }
                    None => self.orphaned = true,
                },
            }
        }
    }
}
