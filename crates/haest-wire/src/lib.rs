//! haest's wire format: how a running conversation talks to another program.
//!
//! One JSON object per line (NDJSON over a pipe or a socket) or per WebSocket
//! message. This crate is the vocabulary and the bookkeeping, with no I/O and
//! no async runtime: the [`Envelope`] every outgoing line has, the [`Outbound`]
//! frames of its own, the [`Inbound`] frames a peer sends, a [`Framer`] that
//! numbers outgoing frames, a [`Ring`] that keeps them for catch-up and says
//! when it lost some, and a [`LineDecoder`] for bytes that arrive in pieces.
//! A program in another language follows this page; a Rust one takes the types.
//!
//! The conversation's own events are haest-core's `LoopEvent`, unchanged: this
//! crate does not depend on haest-core, so an envelope is generic over its body
//! ([`Envelope<B>`]). haest-session puts the two together.
//!
//! # Outgoing: the envelope
//!
//! Every line the conversation writes is an object with these keys, then the
//! body's own (`type` and its fields, flattened in):
//!
//! | Key | What |
//! |---|---|
//! | `v` | the wire version, [`WIRE_V`] |
//! | `sid` | the stream's id: one per running conversation, fixed for its life |
//! | `seq` | the frame's place in the stream: 1, 2, 3, ... never reused |
//! | `ts` | when it was written, ms since the Unix epoch |
//! | `path` | who it is about: `["main"]`, or `["main", "<subagent id>"]` |
//! | `type` | what it is |
//!
//! ```text
//! {"v":1,"sid":"s1","seq":7,"ts":1731790000000,"path":["main"],"type":"text_delta","text":"Hel"}
//! ```
//!
//! A reader of haest's older `--json` output keeps working: every field it
//! read is still there, in the same place.
//!
//! ## Stream frames
//!
//! Each has its own `seq`, and a [`Ring`] keeps it for a peer that catches up:
//!
//! - the conversation's events: `user`, `model_start`, `text_delta`,
//!   `thinking_delta`, `tool_call_start`, `assistant`, `tool_result`, `guard`,
//!   `turn_end` (haest-core's `LoopEvent`; a subagent's carry its path);
//! - [`Outbound::Notice`], [`Outbound::Ask`], [`Outbound::Status`],
//!   [`Outbound::ApprovalAsk`], [`Outbound::ApprovalWithdrawn`],
//!   [`Outbound::SubagentStarted`], [`Outbound::SubagentStopped`].
//!
//! ## Connection frames
//!
//! [`Outbound::Hello`], [`Outbound::Gap`] and [`Outbound::Reply`] belong to
//! one connection. Their `seq` is the last stream frame that connection was
//! sent before them (0 for none): a place to resume from, never a new one.
//!
//! # Incoming: requests
//!
//! A peer sends [`Inbound`] objects: `v`, an `id` the [`Outbound::Reply`]
//! echoes, a `type`, and its fields ([`Request`]):
//!
//! ```text
//! {"v":1,"id":"r1","type":"turn","text":"run the tests"}
//! {"v":1,"id":"r2","type":"control","verb":"interrupt"}
//! {"v":1,"id":"r3","type":"events","after":0,"follow":true}
//! ```
//!
//! [`decode`] checks `v` before anything else: a frame of another version is
//! refused as that ([`DecodeError::Version`]), never read as a frame of this one.
//!
//! # Catching up
//!
//! A peer that connects late, or lost its connection, asks for the frames
//! after the last `seq` it has (`events {after}`). It gets them from the ring,
//! and a `gap` first when the ring no longer holds all of them. The ring keeps
//! the running turn whole, and drops a model call's streamed pieces once the
//! `assistant` frame that holds their text is in: that is not a gap, so a
//! `seq` may be skipped without one. On a gap a peer asks for `history` (the
//! messages, with the `seq` they are current to) and follows from there.
//!
//! # Versions
//!
//! A new frame type, or a new field, is additive: the version stays. A changed
//! shape of an existing one bumps [`WIRE_V`]. A reader skips a `type` it does
//! not know.
//!
//! # Examples
//!
//! The conversation's side: number frames, keep them, catch a peer up.
//!
//! ```
//! use std::sync::Arc;
//!
//! use haest_wire::{Classify, FrameKind, Framer, Level, Outbound, Path, Ring};
//!
//! /// This end's bodies: here only the wire's own.
//! #[derive(serde::Serialize)]
//! struct Body(Outbound);
//!
//! impl Classify for Body {
//!     fn kind(&self) -> FrameKind {
//!         FrameKind::Other
//!     }
//! }
//!
//! let mut framer = Framer::new("s1");
//! let mut ring = Ring::new(2); // keeps the last two frames
//! for text in ["one", "two", "three"] {
//!     let notice = Outbound::Notice { level: Level::Info, text: text.into() };
//!     ring.push(Arc::new(framer.stamp(Path::main(), Body(notice))));
//! }
//!
//! // A peer that has seq 1 gets 2 and 3: nothing lost.
//! let replay = ring.since(1);
//! assert!(!replay.gap);
//! assert_eq!(replay.frames.iter().map(|f| f.seq).collect::<Vec<_>>(), [2, 3]);
//!
//! // A peer with nothing missed 1, which the ring no longer holds: a gap.
//! assert!(ring.since(0).gap);
//!
//! // One line on the wire.
//! let line = serde_json::to_value(&*replay.frames[0]).unwrap();
//! assert_eq!((line["type"].as_str(), line["seq"].as_u64()), (Some("notice"), Some(2)));
//! ```
//!
//! The peer's side: send a request, refuse a frame of another version.
//!
//! ```
//! use haest_wire::{DecodeError, Request, decode};
//!
//! let turn = decode(r#"{"v":1,"id":"r1","type":"turn","text":"hi"}"#).unwrap();
//! assert_eq!(turn.request, Request::Turn { text: "hi".into() });
//!
//! let old = decode(r#"{"v":0,"id":"r1","type":"turn","text":"hi"}"#);
//! assert!(matches!(old, Err(DecodeError::Version { got: 0, .. })));
//! ```

#![cfg_attr(docsrs, feature(doc_cfg))]
#![warn(missing_docs)]

mod words;

mod envelope;
mod frames;
mod inbound;
mod lines;
mod ring;

pub use envelope::{Envelope, Framer, Path, WIRE_V, now_ms};
pub use frames::{Access, Level, Origin, Outbound, Snapshot, State};
pub use inbound::{Answer, Control, DecodeError, Inbound, Request, decode, decode_value};
pub use lines::{LineDecoder, LineTooLong, encode_line};
pub use ring::{Classify, FrameKind, Replay, Ring};
pub use words::UnknownWord;
