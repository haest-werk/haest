//! A haest session served to other programs: the same wire frames over a
//! pipe pair, a Unix control socket or a broker link.
//!
//! A transport is a dumb pipe in front of a session's
//! [`Backend`](haest_session::Backend): frames in, frames out, and the access
//! check. Everything else (queueing, steering, approvals, catch-up after a
//! gap) lives once, in the session. Every connection is served by the same
//! code, so the rules are the same on each:
//!
//! - The first frame is `hello`: the session, its model, the process, and
//!   the [`Access`] the connection has.
//! - Each request gets one `reply` with its `id`; a turn's reply comes when
//!   the turn ends, with its stop reason.
//! - Stream frames come only as asked for: `events {after, follow}` replays
//!   what the ring kept after `after` (a `gap` first if it lost some), then
//!   the reply, then with `follow` every new frame.
//! - A request with another `v`, or one the connection's access does not
//!   allow, is refused with its `id`; nothing else happens.
//! - A peer that does not read falls behind and gets a `gap`; the
//!   conversation never waits on it.
//!
//! The host decides who a connection speaks for, in a [`Grant`]: its access
//! (watch or command) and the [`Origin`] of the input it sends (a person or
//! a program, for the turn gate). No frame can raise either.
//!
//! Nothing here reads a file, a variable or a socket path of its own: the
//! host hands in what a transport needs. Each transport is its own feature,
//! off by default:
//!
//! | Feature | Transport |
//! |---|---|
//! | `stdio` | [`Stdio`]: a parent process drives this one over its stdin and stdout |
//! | `broker` | [`Broker`]: the conversation dials out to a broker over a WebSocket, so a fleet view elsewhere watches (or drives) it |
//! | `unix` | [`UnixSocket`]: programs on this machine reach it over a Unix socket (Unix only); [`running_sessions`] and [`connect_session`] find and reach one |
//!
//! [`Peer`] is the asking side of any connection: `hello` read, requests
//! sent, frames and replies read back.
//!
//! Through the facade this crate is `haest::transport`.
//!
//! # Examples
//!
//! Any line source and sink can carry a connection ([`serve`]); here two
//! in-memory ones, a watch-only peer that asks for a turn.
//!
//! ```
//! use std::sync::Arc;
//!
//! use haest_core::{Agent, AgentDeps, AgentOptions, ShutWorkspace, Tools, testing::ScriptedProvider};
//! use haest_session::{Hub, HubOptions, start};
//! use haest_transport::{Grant, LineSink, LineSource, Start, TransportError, serve};
//! use haest_wire::Origin;
//!
//! /// Lines from a list.
//! struct Lines(Vec<String>);
//! impl LineSource for Lines {
//!     async fn next_line(&mut self) -> Option<Result<String, TransportError>> {
//!         (!self.0.is_empty()).then(|| Ok(self.0.remove(0)))
//!     }
//! }
//!
//! /// Lines into a channel.
//! struct Out(tokio::sync::mpsc::UnboundedSender<String>);
//! impl LineSink for Out {
//!     async fn send_line(&mut self, line: String) -> Result<(), TransportError> {
//!         let _ = self.0.send(line);
//!         Ok(())
//!     }
//! }
//!
//! # #[tokio::main(flavor = "current_thread")]
//! # async fn main() {
//! let model = ScriptedProvider::new([]);
//! let deps = AgentDeps::new(model.resolver(), Tools::new(), "", "/", ShutWorkspace);
//! let session = start(Agent::new(deps, AgentOptions::new("scripted:m")), Hub::new("s1", HubOptions::default()));
//!
//! let asks = Lines(vec![r#"{"v":1,"id":"r1","type":"turn","text":"hi"}"#.into()]);
//! let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
//! serve(Arc::new(session), Grant::watch(Origin::Agent), Start::default(), asks, Out(tx)).await;
//!
//! assert!(rx.recv().await.unwrap().contains(r#""type":"hello""#));
//! let reply = rx.recv().await.unwrap();
//! assert!(reply.contains(r#""id":"r1","ok":false"#), "{reply}");
//! # }
//! ```

#![cfg_attr(docsrs, feature(doc_cfg))]

use std::path::PathBuf;

use haest_wire::{Access, LineTooLong, Origin};
use tokio::task::JoinHandle;

#[cfg(feature = "broker")]
mod broker;
mod link;
mod ndjson;
mod peer;
#[cfg(feature = "stdio")]
mod stdio;
#[cfg(test)]
mod testing;
#[cfg(test)]
mod tests;
#[cfg(all(unix, feature = "unix"))]
mod unix;

#[cfg(feature = "broker")]
#[cfg_attr(docsrs, doc(cfg(feature = "broker")))]
pub use broker::{AUTH_CLOSE, Backoff, Broker};
pub use link::{Ended, LineSink, LineSource, Start, serve};
pub use ndjson::{NdjsonSink, NdjsonSource};
pub use peer::Peer;
#[cfg(feature = "stdio")]
#[cfg_attr(docsrs, doc(cfg(feature = "stdio")))]
pub use stdio::{ProcessStdin, Stdio};
#[cfg(all(unix, feature = "unix"))]
#[cfg_attr(docsrs, doc(cfg(all(unix, feature = "unix"))))]
pub use unix::{
    RunningSession, SocketDiscovery, UnixPeer, UnixSocket, connect_session, running_sessions,
};

/// Who a connection speaks for, as the host decides it: what it may do and
/// whose input it sends. Nothing the peer sends changes it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Grant {
    /// Watch only, or command too (turns, controls, approval answers).
    pub access: Access,
    /// Whose input its turns are, for the turn gate.
    pub origin: Origin,
}

impl Grant {
    /// May only watch: `events`, `snapshot`, `history`.
    pub fn watch(origin: Origin) -> Self {
        Self {
            access: Access::Watch,
            origin,
        }
    }

    /// May watch and drive: turns, controls and approval answers too.
    pub fn command(origin: Origin) -> Self {
        Self {
            access: Access::Command,
            origin,
        }
    }
}

/// A way to serve a session to other programs.
///
/// Several attach to one session at once, each failing on its own; one that
/// ends does not end the session.
pub trait Transport {
    /// Serve `backend` under `grant` until the transport ends, on a task of
    /// its own (in the current tokio runtime).
    ///
    /// # Errors
    ///
    /// [`TransportError`] when it could not start (a socket it cannot bind).
    fn attach(
        self,
        backend: std::sync::Arc<dyn haest_session::Backend>,
        grant: Grant,
    ) -> Result<Attached, TransportError>;
}

/// An attached transport. Dropping it detaches it, as [`detach`](Self::detach) does.
#[derive(Debug)]
pub struct Attached {
    task: Option<JoinHandle<Ended>>,
}

impl Attached {
    /// The transport running on `task`.
    pub fn new(task: JoinHandle<Ended>) -> Self {
        Self { task: Some(task) }
    }

    /// Wait for it to end on its own: why it did.
    pub async fn ended(mut self) -> Ended {
        let Some(task) = self.task.take() else {
            return Ended::Closed;
        };
        match task.await {
            Ok(ended) => ended,
            Err(e) if e.is_panic() => std::panic::resume_unwind(e.into_panic()),
            // The runtime is shutting down.
            Err(_) => Ended::Closed,
        }
    }

    /// Stop serving now. The conversation goes on; frames not yet written
    /// are dropped.
    pub fn detach(mut self) {
        self.stop();
    }

    fn stop(&mut self) {
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}

impl Drop for Attached {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Why a transport could not serve.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum TransportError {
    /// Reading or writing failed.
    #[error(transparent)]
    Io(#[from] std::io::Error),
    /// The peer sent a line past the limit without its newline.
    #[error(transparent)]
    LineTooLong(#[from] LineTooLong),
    /// A file or a socket at this path could not be made, read or reached.
    #[error("{}", path.display())]
    Path {
        /// The path.
        path: PathBuf,
        /// What failed.
        #[source]
        source: std::io::Error,
    },
    /// A running session already serves a socket at this path.
    #[error("{} is served by a running session", .0.display())]
    InUse(PathBuf),
    /// No served session has this id, or an id that starts with it.
    #[error("no session served here has the id {0}")]
    NoSession(String),
    /// More than one served session has an id that starts with this.
    #[error("more than one session served here has an id that starts with {0}")]
    Ambiguous(String),
    /// A line that is not a frame this version reads.
    #[error("a line that is not a frame")]
    NotAFrame(#[source] serde_json::Error),
    /// The first frame was not `hello`.
    #[error("the connection did not start with hello")]
    NotHello,
    /// The connection ended before the reply.
    #[error("the connection ended before its reply")]
    Ended,
    /// The other end refused the request; its reason.
    #[error("refused: {0}")]
    Refused(String),
    /// Not a URL a broker link dials: it takes `ws://` or `wss://`.
    #[error("{0} is not a ws:// or wss:// URL")]
    Url(String),
    /// The broker refused the token: it closed with 4401 or 4408, or
    /// refused the handshake with 401 or 403. The link is not dialed again.
    #[error("the broker refused the link ({code}): {reason}")]
    Auth {
        /// The close code, or the HTTP status.
        code: u16,
        /// What the broker said.
        reason: String,
    },
    /// The WebSocket to the broker failed (dialing, TLS, a read or a write).
    #[error("the broker link failed")]
    WebSocket(#[source] haest_core::error::BoxError),
}
