//! One turn under the driver: its events to the hub, input that arrives
//! steered into it, an interrupt at once, every other request after it.

use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use haest_core::LoopEvent;
use haest_wire::{Origin, State};

use super::{Command, Driver, Leftover, TurnInput, Waiter, snapshot};
use crate::{Control, Driven};

/// Input steered into the running turn: it landed if a user message entered
/// history after it was steered.
struct Steer {
    /// User messages the turn had emitted when it was steered.
    at: usize,
    human: bool,
    done: Option<Waiter>,
}

impl<D: Driven> Driver<D> {
    /// Run `input` as a turn; `waiters` get its outcome.
    pub(super) async fn turn(&mut self, input: TurnInput, mut waiters: Vec<Waiter>) {
        self.publish_status(State::Running);
        let control = self.driven.control();
        let users = Arc::new(AtomicUsize::new(0));
        let mut sink = {
            let (hub, users) = (self.hub.clone(), users.clone());
            move |ev: LoopEvent| {
                if matches!(ev, LoopEvent::User { .. }) {
                    users.fetch_add(1, Ordering::Relaxed);
                }
                hub.event(ev);
            }
        };
        let mut steered: Vec<Steer> = Vec::new();
        let outcome = {
            let run = match &input {
                TurnInput::Human(text) => self.driven.human_turn(text, &mut sink),
                TurnInput::Ready(text) => self.driven.run(text, &mut sink),
            };
            tokio::pin!(run);
            loop {
                tokio::select! {
                    // The turn first: it marks itself running on its first
                    // poll, so an interrupt that comes right after reaches it.
                    biased;
                    end = &mut run => break end,
                    cmd = self.rx.recv(), if !self.orphaned => match cmd {
                        Some(Command::Submit { input, done }) => {
                            control.steer(input.text);
                            let at = users.load(Ordering::Relaxed);
                            let human = input.origin == Origin::Human;
                            steered.push(Steer { at, human, done });
                        }
                        Some(Command::Control { control: Control::Interrupt, reply }) => {
                            control.abort();
                            let _ = reply.send(Ok(()));
                        }
                        Some(Command::Snapshot(tx)) => {
                            let queued = self.queue.len();
                            let now = snapshot(&self.status, State::Running, queued, self.hub.last_seq());
                            let _ = tx.send(now);
                        }
                        Some(cmd @ Command::Close(_)) => {
                            control.abort();
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
        };
        // The turn's own input is its first user message: a steer landed only
        // if one came after it and after the steer. (A steer that arrives
        // while queued input is being turned into a message counts as landed
        // early; it lands at the boundary after the next model call, unless
        // the turn is stopped first.)
        let users = users.load(Ordering::Relaxed);
        let mut left = Leftover {
            human: false,
            waiters: Vec::new(),
            outcome: outcome.clone(),
        };
        for steer in steered {
            if users > steer.at.max(1) {
                waiters.extend(steer.done);
            } else {
                left.human |= steer.human;
                left.waiters.extend(steer.done);
            }
        }
        if left.human || !left.waiters.is_empty() {
            self.leftover = Some(left);
        }
        self.status = self.driven.status();
        // Every frame of the turn is in the stream before anyone hears it
        // ended, so a peer's reply can follow them.
        self.publish_status(State::Idle);
        for waiter in waiters {
            let _ = waiter.send(Ok(outcome.clone()));
        }
    }
}
