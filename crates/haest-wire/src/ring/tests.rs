use super::*;
use crate::Framer;

/// A test body: what the ring needs to know, and nothing else.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Ev {
    Start,
    Piece,
    Answer,
    End,
    Note,
}

impl Classify for Ev {
    fn kind(&self) -> FrameKind {
        match self {
            Ev::Start => FrameKind::TurnStart,
            Ev::Piece => FrameKind::Delta,
            Ev::Answer => FrameKind::Answer,
            Ev::End => FrameKind::TurnEnd,
            Ev::Note => FrameKind::Other,
        }
    }
}

fn ring_of(capacity: usize, evs: &[(Path, Ev)]) -> Ring<Ev> {
    let mut framer = Framer::new("s");
    let mut ring = Ring::new(capacity);
    for (path, ev) in evs {
        ring.push(framer.stamp(path.clone(), *ev));
    }
    ring
}

fn notes(n: usize) -> Vec<(Path, Ev)> {
    vec![(Path::main(), Ev::Note); n]
}

fn seqs(r: &Replay<Ev>) -> Vec<u64> {
    r.frames.iter().map(|f| f.seq).collect()
}

/// replay-buffer.test.ts: frames strictly after the seq asked for.
#[test]
fn since_gives_the_frames_strictly_after_the_seq() {
    let ring = ring_of(10, &notes(3));
    let r = ring.since(1);
    assert_eq!((seqs(&r), r.gap), (vec![2, 3], false));
}

/// replay-buffer.test.ts and agent-events.test.ts: past the capacity the
/// oldest go, and a replay into them says so.
#[test]
fn an_eviction_is_a_gap_for_a_peer_before_it() {
    let ring = ring_of(2, &notes(4));
    let r = ring.since(1);
    assert_eq!((seqs(&r), r.gap), (vec![3, 4], true));
    assert!(ring.since(0).gap);
    let exact = ring.since(2);
    assert_eq!(
        (seqs(&exact), exact.gap),
        (vec![3, 4], false),
        "3 is exactly next"
    );
}

/// Caught up, or nothing yet: nothing, and no gap.
#[test]
fn a_peer_that_has_everything_gets_nothing_and_no_gap() {
    let ring = ring_of(5, &notes(5));
    let r = ring.since(5);
    assert_eq!((seqs(&r), r.gap), (vec![], false));
    let empty = ring_of(5, &[]);
    let r = empty.since(0);
    assert_eq!((seqs(&r), r.gap, empty.len()), (vec![], false, 0));
}

/// The running turn is kept whole past the capacity; once it ends, the ring
/// is trimmed back.
#[test]
fn the_running_turn_is_never_evicted() {
    let mut evs = notes(3);
    evs.push((Path::main(), Ev::Start));
    evs.extend(notes(5));
    let ring = ring_of(2, &evs);
    let r = ring.since(3);
    assert_eq!(seqs(&r), [4, 5, 6, 7, 8, 9], "the turn, whole");
    assert!(!r.gap, "the turn started right after 3");
    assert!(ring.since(0).gap, "the notes before it were trimmed");

    let mut ring = ring;
    let mut framer = Framer::new("s").starting_after(9);
    // A frame the caller shares (a broadcast) goes in as its Arc.
    let end = Arc::new(framer.stamp(Path::main(), Ev::End));
    ring.push(end.clone());
    assert!(Arc::ptr_eq(&ring.since(9).frames[0], &end));
    assert_eq!(ring.len(), 2);
    assert!(ring.since(3).gap);
}

/// A smaller capacity evicts at once, a gap like any eviction; the running
/// turn still stays whole.
#[test]
fn a_smaller_capacity_evicts_at_once() {
    let mut ring = ring_of(10, &notes(5));
    ring.set_capacity(2);
    assert_eq!((ring.capacity(), ring.len()), (2, 2));
    let r = ring.since(0);
    assert_eq!((seqs(&r), r.gap), (vec![4, 5], true));
    ring.set_capacity(0);
    assert_eq!(ring.capacity(), 1, "at least one");

    let mut evs = notes(2);
    evs.push((Path::main(), Ev::Start));
    evs.extend(notes(3));
    let mut ring = ring_of(10, &evs);
    ring.set_capacity(1);
    assert_eq!(
        seqs(&ring.since(0)),
        [2, 3, 4, 5, 6],
        "one frame before the turn, and the turn whole"
    );
}

/// A model call's pieces go once its answer is in; their seqs are skipped
/// without a gap. Another path's pieces stay.
#[test]
fn deltas_go_when_their_answer_comes_without_a_gap() {
    let sub = Path::subagent("a1");
    let ring = ring_of(
        100,
        &[
            (Path::main(), Ev::Start),
            (Path::main(), Ev::Piece),
            (sub.clone(), Ev::Piece),
            (Path::main(), Ev::Piece),
            (Path::main(), Ev::Answer),
        ],
    );
    let r = ring.since(0);
    assert_eq!(seqs(&r), [1, 3, 5]);
    assert!(!r.gap);
    assert_eq!(r.frames[1].path, sub);
}

/// Only the conversation's own turn is kept whole: a subagent's turn start
/// does not hold the ring.
#[test]
fn a_subagents_turn_does_not_hold_the_ring() {
    let sub = Path::subagent("a1");
    let mut evs = vec![(sub, Ev::Start)];
    evs.extend(notes(4));
    let ring = ring_of(2, &evs);
    assert_eq!(seqs(&ring.since(0)), [4, 5]);
}
