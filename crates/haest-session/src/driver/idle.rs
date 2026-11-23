//! The idle tick ([`Driven::idle_tick`]): between turns, when the
//! conversation says one is due.

use tokio_util::sync::CancellationToken;

use super::{Command, Driver, last_said};
use crate::Driven;

/// Whether a request needs the conversation: an idle tick stops for it.
fn needs_the_conversation<D>(cmd: &Command<D>) -> bool {
    matches!(
        cmd,
        Command::Submit { .. } | Command::Control { .. } | Command::With(_) | Command::Close(_)
    )
}

impl<D: Driven> Driver<D> {
    /// The conversation's idle tick. A snapshot is answered at once, as
    /// during a host's job; a request that needs the conversation (input, a
    /// change, a job, a close) stops the tick and waits for its end; any
    /// other just waits. Every handle gone stops it too. What it changed is
    /// published when it ends.
    pub(super) async fn idle_tick(&mut self) {
        let cancel = CancellationToken::new();
        {
            let mut run = self.driven.idle_tick(&cancel);
            loop {
                tokio::select! {
                    biased;
                    () = &mut run => break,
                    cmd = self.rx.recv(), if !self.orphaned => match cmd {
                        Some(Command::Snapshot(tx)) => {
                            let _ = tx.send(last_said(&self.status, &self.queue, &self.deferred, &self.hub));
                        }
                        Some(cmd) => {
                            if needs_the_conversation(&cmd) {
                                cancel.cancel();
                            }
                            self.deferred.push_back(cmd);
                        }
                        None => {
                            self.orphaned = true;
                            cancel.cancel();
                        }
                    },
                }
            }
        }
        self.refresh();
    }
}
