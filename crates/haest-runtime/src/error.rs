//! Why a conversation could not be built ([`BuildError`]), and why its
//! session file could not be opened or created ([`SessionFileError`]).

use std::{io, path::PathBuf};

use haest_core::{ModelError, session::InvalidSessionId};

use crate::world::WorkspaceError;

/// Why [`build`](crate::build) failed. It fails before any MCP server or mod
/// starts and before a new session's file is written, so there is nothing to
/// undo.
///
/// The `Display` of each is its cause's own; [`std::error::Error::source`]
/// goes on down the chain.
///
/// A `match` on it has a `_` arm: haest may add a cause in a minor release.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum BuildError {
    /// The session to resume is not there, cannot be read or is in use by
    /// another run, or a new session's file cannot be created.
    #[error(transparent)]
    SessionFile(#[from] SessionFileError),
    /// The model the conversation would start on cannot be used: the spec is
    /// malformed, its provider unknown, or the host hands in no key for it.
    #[error(transparent)]
    Model(#[from] ModelError),
    /// The workspace the settings pick cannot open.
    #[error(transparent)]
    Workspace(#[from] WorkspaceError),
}

/// Why the session file a run writes could not be opened, read or created.
/// (haest-session's `SessionError` is a running session's; this is its
/// file's.)
///
/// A `match` on it has a `_` arm: haest may add a cause in a minor release.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum SessionFileError {
    /// The id to resume is not a session id; no file was looked for.
    #[error(transparent)]
    InvalidId(#[from] InvalidSessionId),
    /// A resume of the latest session ([`Resume::Latest`](crate::Resume::Latest)),
    /// and no saved session started in this dir.
    #[error("no saved session in {} to resume", cwd.display())]
    NoneToResume {
        /// The settings' `cwd`.
        cwd: PathBuf,
    },
    /// A resume by id ([`Resume::Id`](crate::Resume::Id)), and no session has it.
    #[error("no saved session \"{id}\"")]
    NotFound {
        /// The id, as given.
        id: String,
    },
    /// Another run is writing the session: it holds the session's lock
    /// until it ends (a second `haest -r` while the first still runs).
    /// Nothing was read or written.
    #[error("session \"{id}\" is in use by another run")]
    InUse {
        /// The session's id.
        id: String,
    },
    /// The saved session's file could not be read.
    #[error("reading the session {}", path.display())]
    Read {
        /// The session file.
        path: PathBuf,
        /// What the read hit.
        #[source]
        source: io::Error,
    },
    /// A new session's file could not be created.
    #[error("creating the session file {}", path.display())]
    Create {
        /// The session file.
        path: PathBuf,
        /// What the write hit.
        #[source]
        source: io::Error,
    },
}
