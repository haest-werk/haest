//! [`Ring`]: the stream frames kept for a peer that catches up, and whether
//! some it asks for are gone.

use std::{collections::VecDeque, sync::Arc};

use crate::{Envelope, Path};

/// What a frame is to the [`Ring`]: whether it may be dropped, and when.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameKind {
    /// A streamed piece of a model call (text, reasoning, a tool call
    /// beginning): dropped once the [`Answer`](Self::Answer) on its path is in,
    /// since that frame holds it.
    Delta,
    /// A model call's whole answer: the deltas before it on its path go.
    Answer,
    /// The conversation's turn starts: from here to its end nothing is evicted.
    TurnStart,
    /// The conversation's turn ended: its frames may be evicted again.
    TurnEnd,
    /// Anything else.
    Other,
}

/// How a body tells the [`Ring`] what it is. Only frames whose path is
/// `["main"]` start or end the turn the ring keeps whole.
pub trait Classify {
    /// What this frame is to the ring.
    fn kind(&self) -> FrameKind;
}

/// What [`Ring::since`] gives back.
#[derive(Debug, Clone)]
pub struct Replay<B> {
    /// The frames kept after the seq asked for, oldest first.
    pub frames: Vec<Arc<Envelope<B>>>,
    /// Some frames after that seq were evicted: the peer has a hole.
    pub gap: bool,
}

/// The last stream frames, for a peer that connects late or lost its
/// connection: [`since`](Self::since) a seq, and a `gap` when some are gone.
///
/// - It keeps `capacity` frames outside the running turn; the running turn
///   (from a [`TurnStart`](FrameKind::TurnStart) to its
///   [`TurnEnd`](FrameKind::TurnEnd), on the path `["main"]`) is never
///   evicted, so a peer that catches up mid-turn sees it whole.
/// - A model call's [`Delta`](FrameKind::Delta)s go when its
///   [`Answer`](FrameKind::Answer) comes in: the answer holds their text, so
///   their seqs are skipped without a gap.
/// - Only an eviction makes a gap.
///
/// ```
/// use haest_wire::{Classify, FrameKind, Framer, Path, Ring};
///
/// #[derive(Clone, Copy)]
/// enum Ev { Start, Piece, Answer, End }
///
/// impl Classify for Ev {
///     fn kind(&self) -> FrameKind {
///         match self {
///             Ev::Start => FrameKind::TurnStart,
///             Ev::Piece => FrameKind::Delta,
///             Ev::Answer => FrameKind::Answer,
///             Ev::End => FrameKind::TurnEnd,
///         }
///     }
/// }
///
/// let mut framer = Framer::new("s");
/// let mut ring = Ring::new(1);
/// for ev in [Ev::Start, Ev::Piece, Ev::Piece, Ev::Answer] {
///     ring.push(framer.stamp(Path::main(), ev));
/// }
/// // Mid-turn the whole turn is kept, past the capacity; the pieces went
/// // when the answer came, and that is not a gap.
/// let replay = ring.since(0);
/// assert_eq!(replay.frames.iter().map(|f| f.seq).collect::<Vec<_>>(), [1, 4]);
/// assert!(!replay.gap);
///
/// ring.push(framer.stamp(Path::main(), Ev::End));
/// // The turn ended: down to the capacity, and the start is gone.
/// assert!(ring.since(0).gap);
/// assert_eq!(ring.len(), 1);
/// ```
#[derive(Debug)]
pub struct Ring<B> {
    frames: VecDeque<Arc<Envelope<B>>>,
    capacity: usize,
    /// The highest seq evicted; a peer before it has a hole.
    evicted_through: u64,
    /// The seq the running turn started at; None between turns.
    running_from: Option<u64>,
}

impl<B> Ring<B> {
    /// A ring that keeps `capacity` frames (at least 1) besides the running turn.
    pub fn new(capacity: usize) -> Self {
        Self {
            frames: VecDeque::new(),
            capacity: capacity.max(1),
            evicted_through: 0,
            running_from: None,
        }
    }

    /// How many frames it keeps besides the running turn.
    pub fn capacity(&self) -> usize {
        self.capacity
    }

    /// Keep `capacity` frames (at least 1) besides the running turn from now
    /// on: a smaller one evicts the oldest at once, and that is a gap as any
    /// eviction is.
    pub fn set_capacity(&mut self, capacity: usize) {
        self.capacity = capacity.max(1);
        self.evict();
    }

    /// The frames it holds now.
    pub fn len(&self) -> usize {
        self.frames.len()
    }

    /// Whether it holds none.
    pub fn is_empty(&self) -> bool {
        self.frames.is_empty()
    }

    /// The frames kept after `after`, oldest first, and whether some after
    /// it were evicted (a peer at `after` has a hole).
    pub fn since(&self, after: u64) -> Replay<B> {
        let first = self.frames.partition_point(|f| f.seq <= after);
        Replay {
            frames: self.frames.range(first..).cloned().collect(),
            gap: after < self.evicted_through,
        }
    }

    /// Evict the oldest frames outside the running turn past the capacity.
    fn evict(&mut self) {
        let outside = match self.running_from {
            Some(from) => self.frames.partition_point(|f| f.seq < from),
            None => self.frames.len(),
        };
        for _ in self.capacity..outside {
            if let Some(gone) = self.frames.pop_front() {
                self.evicted_through = self.evicted_through.max(gone.seq);
            }
        }
    }
}

impl<B: Classify> Ring<B> {
    /// Keep `frame`, the stream's newest; evict the oldest past the capacity.
    /// Takes the frame or an `Arc` of it: one the caller also hands out (a
    /// broadcast) is shared, not copied.
    pub fn push(&mut self, frame: impl Into<Arc<Envelope<B>>>) {
        let frame = frame.into();
        let main = frame.path.is_main();
        match frame.body.kind() {
            FrameKind::Answer => drop_deltas(&mut self.frames, &frame.path),
            FrameKind::TurnStart if main => self.running_from = Some(frame.seq),
            FrameKind::TurnEnd if main => self.running_from = None,
            _ => {}
        }
        self.frames.push_back(frame);
        self.evict();
    }
}

/// Drop the deltas on `path`: its answer holds them now.
fn drop_deltas<B: Classify>(frames: &mut VecDeque<Arc<Envelope<B>>>, path: &Path) {
    frames.retain(|f| !(f.body.kind() == FrameKind::Delta && &f.path == path));
}

#[cfg(test)]
mod tests;
