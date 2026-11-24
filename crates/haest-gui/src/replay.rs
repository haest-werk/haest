//! A recorded stream played back as if a haest sent it: one frame per line,
//! the NDJSON `haest ctl events <id> --json` writes. For deterministic
//! screenshots and for looking at a session after the fact.

use std::{
    path::{Path, PathBuf},
    time::Duration,
};

use haest::session::{Body, Frame};
use haest::wire::{Access, Outbound};
use thiserror::Error;

use crate::link::{Event, Found};

/// The frames in `text`, one per line; lines that are not frames are skipped.
pub fn parse(text: &str) -> Vec<Frame> {
    text.lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| serde_json::from_str::<Frame>(l).ok())
        .collect()
}

/// The session a recording is of, as discovery would have found it.
pub fn found(frames: &[Frame], access: Access) -> Option<Found> {
    let first = frames.first()?;
    let model = frames
        .iter()
        .find_map(|f| match &f.body {
            Body::Wire(Outbound::Status(s)) => Some(s.model_spec.clone()),
            _ => None,
        })
        .unwrap_or_default();
    Some(Found {
        sid: first.sid.clone(),
        session: String::new(),
        model,
        pid: 0,
        access,
        started_ms: first.ts,
    })
}

/// The events a live link would have sent for `frames`, in order.
pub fn events(frames: Vec<Frame>, access: Access) -> Vec<Event> {
    let Some(found) = found(&frames, access) else {
        return Vec::new();
    };
    let sid = found.sid.clone();
    let mut out = vec![
        Event::Sessions(vec![found.clone()]),
        Event::Attached {
            sid: sid.clone(),
            found,
        },
    ];
    out.extend(frames.into_iter().map(|frame| Event::Frame {
        sid: sid.clone(),
        frame: Box::new(frame),
    }));
    out
}

/// Why a recording does not play.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum ReplayError {
    /// The file does not read.
    #[error("cannot read {}", path.display())]
    Read {
        /// The recording.
        path: PathBuf,
        /// Why it does not read.
        #[source]
        source: std::io::Error,
    },
    /// The file holds no frame.
    #[error("{}: no frames in it", path.display())]
    Empty {
        /// The recording.
        path: PathBuf,
    },
}

/// Play the recording at `path` into `tx`, keeping its pace (each pause
/// capped at `max_pause`; zero plays it at once).
///
/// # Errors
///
/// [`ReplayError::Read`] when the file does not read, [`ReplayError::Empty`]
/// when no line of it is a frame.
pub async fn play(
    path: &Path,
    tx: async_channel::Sender<Event>,
    max_pause: Duration,
) -> Result<(), ReplayError> {
    let text = tokio::fs::read_to_string(path)
        .await
        .map_err(|source| ReplayError::Read {
            path: path.to_path_buf(),
            source,
        })?;
    let frames = parse(&text);
    if frames.is_empty() {
        return Err(ReplayError::Empty {
            path: path.to_path_buf(),
        });
    }
    let mut last_ts: Option<u64> = None;
    for ev in events(frames, Access::Watch) {
        if let Event::Frame { frame, .. } = &ev {
            if let Some(prev) = last_ts {
                let pause = Duration::from_millis(frame.ts.saturating_sub(prev)).min(max_pause);
                if !pause.is_zero() {
                    tokio::time::sleep(pause).await;
                }
            }
            last_ts = Some(frame.ts);
        }
        if tx.send(ev).await.is_err() {
            break;
        }
    }
    Ok(())
}
