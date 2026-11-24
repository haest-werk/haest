//! [`AcpError`]: why serving a client ended in failure.

use haest_core::error::BoxError;

/// Why [`AcpAgent::serve`](crate::AcpAgent::serve) ended in failure. A
/// client that closes the connection is no failure: `serve` returns `Ok`.
/// More cases may come: a `match` has a `_` arm.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum AcpError {
    /// The connection to the client failed: its bytes could not be read or
    /// written, or a handler could not go on. The protocol library's error
    /// is the source.
    #[error("the connection to the ACP client failed")]
    Connection(#[source] BoxError),
}
