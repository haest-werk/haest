//! Why a server did not connect: [`McpStartError`], kept beside the server's
//! name in an [`McpFailure`].

use std::{io, sync::Arc, time::Duration};

use haest_core::error::BoxError;
use haest_http::HttpError;

/// A server that did not connect, and why: what [`McpHub::failures`](super::McpHub::failures) lists.
///
/// Cheap to clone: the error is shared.
#[derive(Debug, Clone)]
pub struct McpFailure {
    name: String,
    error: Arc<McpStartError>,
}

impl McpFailure {
    pub(crate) fn new(name: &str, error: McpStartError) -> Self {
        Self {
            name: name.to_string(),
            error: Arc::new(error),
        }
    }

    /// The server's name, as configured.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Why it failed. Show it with [`report`](haest_core::error::report)
    /// (`report(failure.error())`): its cause says what broke.
    pub fn error(&self) -> &McpStartError {
        &self.error
    }
}

/// Why one server did not start. The text of each is its own; the reason
/// under it (the OS's, rmcp's, the HTTP library's) is its
/// [`source`](std::error::Error::source).
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum McpStartError {
    /// The stdio server's program did not start.
    #[error("spawn {command}")]
    Spawn {
        /// The program, as configured.
        command: String,
        /// Why the OS did not start it.
        source: io::Error,
    },
    /// A program named by a bare command was not found, and the server was
    /// handed no `PATH` ([`McpHub::new`](super::McpHub::new) hands none).
    #[error(
        "spawn {command} (the server was given no PATH, so a bare command is looked up only in the system's default directories: hand PATH in with McpHub::with_env or the server's env, or name the program by its path)"
    )]
    NotOnPath {
        /// The program, as configured.
        command: String,
        /// The OS's not-found error.
        source: io::Error,
    },
    /// An HTTP server's header has a name HTTP does not take.
    #[error("header \"{name}\" is not a valid name")]
    HeaderName {
        /// The header, as configured.
        name: String,
        /// Why HTTP does not take it.
        source: BoxError,
    },
    /// An HTTP server's header has a value HTTP cannot carry.
    #[error("header \"{name}\" has a value HTTP cannot carry")]
    HeaderValue {
        /// The header, as configured.
        name: String,
        /// Why HTTP cannot carry it.
        source: BoxError,
    },
    /// The HTTP client could not be built (its TLS set-up failed).
    #[error("the HTTP client could not be built")]
    Client(#[source] HttpError),
    /// The server did not complete MCP's `initialize` (the connection broke,
    /// or it answered with an error).
    #[error("MCP initialize failed")]
    Initialize(#[source] BoxError),
    /// `initialize` did not finish within this long ([`CONNECT_TIMEOUT`](super::CONNECT_TIMEOUT)).
    #[error("MCP initialize timed out after {}ms", .0.as_millis())]
    InitializeTimedOut(Duration),
    /// The server did not list its tools.
    #[error("listing its tools failed")]
    ListTools(#[source] BoxError),
    /// Listing the tools did not finish within this long ([`CONNECT_TIMEOUT`](super::CONNECT_TIMEOUT)).
    #[error("listTools timed out after {}ms", .0.as_millis())]
    ListToolsTimedOut(Duration),
}
