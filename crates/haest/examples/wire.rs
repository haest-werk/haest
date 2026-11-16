//! Both ends of haest's wire format, in one process: a conversation numbers
//! its frames and keeps them in a ring; a peer reads the lines as they arrive
//! in pieces, notices the hole when it reconnects too late, and asks for
//! what it needs.
//!
//! `cargo run -p haest --example wire --features wire`
//!
//! A real peer reads the same lines from `haest --json -p "..."`.

use std::sync::Arc;

use haest::wire::{
    Classify, DecodeError, Envelope, FrameKind, Framer, Inbound, LineDecoder, Outbound, Path,
    Request, Ring, decode, encode_line,
};
use serde::Serialize;
use serde_json::Value;

/// What this conversation sends: here only the wire's own frames.
#[derive(Serialize)]
struct Body(Outbound);

impl Classify for Body {
    fn kind(&self) -> FrameKind {
        FrameKind::Other
    }
}

fn notice(text: &str) -> Body {
    Body(Outbound::Notice {
        level: haest::wire::Level::Info,
        text: text.into(),
    })
}

fn main() {
    // The conversation's side: one framer per stream, a ring of the last 3.
    let mut framer = Framer::new("example-stream");
    let mut ring = Ring::new(3);
    let mut wire = String::new();
    for text in ["starting", "reading files", "running tests", "done"] {
        let frame = Arc::new(framer.stamp(Path::main(), notice(text)));
        wire.push_str(&encode_line(&*frame));
        ring.push(frame);
    }

    // The peer's side: the bytes arrive in odd pieces; whole lines come out.
    let mut lines = LineDecoder::new();
    let mut last_seq = 0;
    for piece in wire.as_bytes().chunks(7) {
        for line in lines.push(piece).expect("short lines") {
            // A reader that knows no Rust types: the envelope's keys, the rest as JSON.
            let frame: Envelope<Value> = serde_json::from_str(&line).expect("a frame");
            println!(
                "seq {} {}: {}",
                frame.seq, frame.body["type"], frame.body["text"]
            );
            last_seq = frame.seq;
        }
    }
    assert_eq!(last_seq, 4);

    // A peer that dropped out after seq 0 comes back: the ring kept only 2..4.
    let replay = ring.since(0);
    println!(
        "catching up from 0: {} frames, gap: {}",
        replay.frames.len(),
        replay.gap
    );
    assert!(
        replay.gap,
        "seq 1 is gone: the peer must ask for the history"
    );

    // What that peer sends next, and how a frame of another version is refused.
    let ask = Inbound::new("r1", Request::History);
    println!("the peer sends: {}", encode_line(&ask).trim_end());
    let stale = r#"{"v":0,"id":"r2","type":"history"}"#;
    match decode(stale) {
        Err(e @ DecodeError::Version { .. }) => println!("refused: {e}"),
        other => panic!("a stale frame must be refused, got {other:?}"),
    }
}

/// `cargo test` runs the example, so a change that breaks it fails the suite.
#[test]
fn the_example_runs() {
    main();
}
