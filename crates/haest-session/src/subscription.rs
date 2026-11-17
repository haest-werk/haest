//! [`Subscription`]: one subscriber's view of a stream, caught up from the
//! ring and then live, with a `gap` for whatever it could not get.

use std::{collections::VecDeque, sync::Arc};

use haest_wire::{Envelope, Outbound, Replay};
use tokio::sync::broadcast::{self, error::RecvError};

use crate::{Body, Frame};

/// One subscriber's frames: those the ring kept after the seq it asked for,
/// then every new one, in seq order with none twice.
///
/// When frames it should have had are gone (the ring evicted them, or it fell
/// further behind than the hub's buffer) it gets ONE `gap` frame saying after
/// which seq, and goes on from the next frame it can have. A gap frame is
/// this subscriber's own: its seq is where the subscriber stands, the last
/// frame it was given or the seq it started from (one past the stream's end
/// starts from its end).
///
/// Made by [`Hub::subscribe`](crate::Hub::subscribe) or
/// [`Backend::subscribe`](crate::Backend::subscribe).
#[derive(Debug)]
pub struct Subscription {
    sid: String,
    /// The last stream frame given (or the seq it started after).
    last: u64,
    backlog: VecDeque<Arc<Frame>>,
    rx: broadcast::Receiver<Arc<Frame>>,
}

impl Subscription {
    pub(crate) fn new(
        sid: String,
        after: u64,
        replay: Replay<Body>,
        rx: broadcast::Receiver<Arc<Frame>>,
    ) -> Self {
        let mut backlog = VecDeque::with_capacity(replay.frames.len() + 1);
        if replay.gap {
            backlog.push_back(gap(&sid, after, None));
        }
        backlog.extend(replay.frames);
        Self {
            sid,
            last: after,
            backlog,
            rx,
        }
    }

    /// Where this subscriber stands, the seq of the last stream frame given
    /// so far or the one it started from: where to resume from.
    pub fn last_seq(&self) -> u64 {
        self.last
    }

    /// Everything the ring held for it was given (the gap first, if any):
    /// what comes next is live.
    pub fn caught_up(&self) -> bool {
        self.backlog.is_empty()
    }

    /// The next frame; None once the stream is closed and every frame it
    /// held was given. Cancel-safe: in a `select!`, a frame is never lost.
    pub async fn recv(&mut self) -> Option<Arc<Frame>> {
        if let Some(frame) = self.backlog.pop_front() {
            return Some(self.given(frame));
        }
        loop {
            match self.rx.recv().await {
                // Already given from the ring.
                Ok(frame) if frame.seq <= self.last => {}
                Ok(frame) => return Some(self.given(frame)),
                Err(RecvError::Lagged(missed)) => {
                    return Some(gap(&self.sid, self.last, Some(missed)));
                }
                Err(RecvError::Closed) => return None,
            }
        }
    }

    /// `frame` goes out: a stream frame moves the resume point.
    fn given(&mut self, frame: Arc<Frame>) -> Arc<Frame> {
        if !matches!(frame.body, Body::Wire(Outbound::Gap { .. })) {
            self.last = self.last.max(frame.seq);
        }
        frame
    }
}

/// This subscriber's own gap frame: the hole is after `after`.
fn gap(sid: &str, after: u64, missed: Option<u64>) -> Arc<Frame> {
    let body = Body::Wire(Outbound::Gap { after, missed });
    Arc::new(Envelope::connection(sid, after, body))
}
