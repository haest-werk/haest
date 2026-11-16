//! [`Envelope`]: the keys every outgoing line has, around its body; the
//! [`Framer`] that numbers them; [`Path`], who a frame is about.

use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

/// The wire version this crate speaks. A changed shape of an existing frame
/// bumps it; a new frame type or field does not.
pub const WIRE_V: u32 = 1;

/// Milliseconds since the Unix epoch, now: what a [`Framer`] stamps as `ts`
/// unless given another clock. 0 if the system clock is before the epoch.
pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

/// Who a frame is about: `["main"]` for the conversation itself,
/// `["main", "<subagent id>"]` for one of its subagents.
///
/// ```
/// use haest_wire::Path;
///
/// assert!(Path::main().is_main());
/// let sub = Path::subagent("explore-1");
/// assert_eq!(serde_json::to_string(&sub).unwrap(), r#"["main","explore-1"]"#);
/// assert!(!sub.is_main());
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Path(pub Vec<String>);

impl Path {
    /// The conversation's own name in a path.
    pub const MAIN: &'static str = "main";

    /// `["main"]`.
    pub fn main() -> Self {
        Self(vec![Self::MAIN.to_string()])
    }

    /// `["main", id]`: a subagent of the conversation.
    pub fn subagent(id: impl Into<String>) -> Self {
        Self(vec![Self::MAIN.to_string(), id.into()])
    }

    /// Whether this is `["main"]`.
    pub fn is_main(&self) -> bool {
        self.0.len() == 1 && self.0[0] == Self::MAIN
    }
}

impl Default for Path {
    fn default() -> Self {
        Self::main()
    }
}

/// One outgoing line: the stream's keys, then the body's own fields
/// flattened in (its `type` among them).
///
/// `B` is whatever this end sends: the conversation's events, the wire's own
/// [`Outbound`](crate::Outbound) frames, or an enum of both. A [`Framer`]
/// makes stream frames; [`Envelope::connection`] makes a connection's own.
///
/// ```
/// use haest_wire::{Envelope, Framer, Level, Outbound, Path};
///
/// let mut framer = Framer::new("s1").with_clock(|| 1000);
/// let frame = framer.stamp(Path::main(), Outbound::Ask { text: "Allow?".into() });
/// let line = serde_json::to_string(&frame).unwrap();
/// assert_eq!(line, r#"{"v":1,"sid":"s1","seq":1,"ts":1000,"path":["main"],"type":"ask","text":"Allow?"}"#);
///
/// let back: Envelope<Outbound> = serde_json::from_str(&line).unwrap();
/// assert_eq!(back, frame);
/// ```
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Envelope<B> {
    /// The wire version, [`WIRE_V`].
    pub v: u32,
    /// The stream's id: one per running conversation, fixed for its life.
    pub sid: String,
    /// A stream frame's place in the stream (1, 2, ...); for a connection
    /// frame, the last stream frame that connection was sent before it.
    pub seq: u64,
    /// When it was made, ms since the Unix epoch.
    pub ts: u64,
    /// Who it is about.
    pub path: Path,
    /// What it is: its `type` and fields, flattened into the line.
    #[serde(flatten)]
    pub body: B,
}

impl<B> Envelope<B> {
    /// A connection's own frame (hello, gap, reply): about the conversation,
    /// stamped now, at `seq`, the last stream frame that connection was sent.
    pub fn connection(sid: impl Into<String>, seq: u64, body: B) -> Self {
        Self {
            v: WIRE_V,
            sid: sid.into(),
            seq,
            ts: now_ms(),
            path: Path::main(),
            body,
        }
    }
}

/// Numbers one stream's frames: every [`stamp`](Self::stamp) is the next
/// `seq`, with the stream's id and the time.
///
/// One framer per stream, so `seq` never repeats. Its clock is [`now_ms`]
/// unless [`with_clock`](Self::with_clock) gives another (a test's).
///
/// ```
/// use haest_wire::{Framer, Outbound, Path};
///
/// let mut framer = Framer::new("s1").starting_after(41);
/// let frame = framer.stamp(Path::main(), Outbound::Ask { text: "?".into() });
/// assert_eq!((frame.seq, framer.seq()), (42, 42));
/// ```
#[derive(Debug, Clone)]
pub struct Framer {
    sid: String,
    seq: u64,
    clock: fn() -> u64,
}

impl Framer {
    /// A framer for the stream `sid`; its first frame is seq 1.
    pub fn new(sid: impl Into<String>) -> Self {
        Self {
            sid: sid.into(),
            seq: 0,
            clock: now_ms,
        }
    }

    /// The framer, its next frame after `seq` (a stream that resumes).
    pub fn starting_after(mut self, seq: u64) -> Self {
        self.seq = seq;
        self
    }

    /// The framer with another clock for `ts`.
    pub fn with_clock(mut self, clock: fn() -> u64) -> Self {
        self.clock = clock;
        self
    }

    /// The stream's id.
    pub fn sid(&self) -> &str {
        &self.sid
    }

    /// The seq of the last frame stamped; 0 before the first.
    pub fn seq(&self) -> u64 {
        self.seq
    }

    /// `body`, about `path`, as the stream's next frame.
    pub fn stamp<B>(&mut self, path: Path, body: B) -> Envelope<B> {
        self.seq += 1;
        Envelope {
            v: WIRE_V,
            sid: self.sid.clone(),
            seq: self.seq,
            ts: (self.clock)(),
            path,
            body,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Outbound, decode};

    /// Port of framer.test.ts: stamps the envelope and counts up.
    #[test]
    fn a_framer_stamps_the_envelope_and_counts_up() {
        static T: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(100);
        let tick = || T.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
        let mut f = Framer::new("sess_1").with_clock(tick);
        let a = f.stamp(Path::main(), Outbound::Ask { text: "hi".into() });
        assert_eq!(
            (a.v, a.sid.as_str(), a.seq, a.ts),
            (WIRE_V, "sess_1", 1, 101)
        );
        let b = f.stamp(Path::subagent("x"), Outbound::Ask { text: "yo".into() });
        assert_eq!((b.seq, f.seq()), (2, 2));
        assert_eq!(b.path, Path::subagent("x"));
    }

    #[test]
    fn a_framer_resumes_after_a_given_seq() {
        let mut f = Framer::new("s").starting_after(41).with_clock(|| 0);
        assert_eq!(f.stamp(Path::main(), ()).seq, 42);
    }

    /// The default clock stamps the real time.
    #[test]
    fn the_default_clock_is_the_time_now() {
        let before = now_ms();
        let frame = Framer::new("s").stamp(Path::main(), ());
        assert!(frame.ts >= before && frame.ts <= now_ms());
        assert_eq!(frame.seq, 1);
    }

    /// A connection's frame does not take a place in the stream.
    #[test]
    fn a_connection_frame_carries_the_seq_it_is_given() {
        let gap = Envelope::connection(
            "s",
            7,
            Outbound::Gap {
                after: 7,
                missed: None,
            },
        );
        assert_eq!((gap.seq, gap.path.is_main()), (7, true));
    }

    /// A frame a peer writes with this end's framer decodes on the other end.
    #[test]
    fn a_stamped_request_line_decodes() {
        let line =
            serde_json::json!({"v": WIRE_V, "id": "x", "type": "control", "verb": "interrupt"});
        assert!(decode(&line.to_string()).is_ok());
    }
}
