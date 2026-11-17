//! haest's HTTP seam: the [`HttpClient`] trait haest's parts send through,
//! and its default.
//!
//! The model calls (haest-providers), GenerateImage (haest-tools) and the
//! GitHub tracker (haest-issues) build a [`Request`] and hand it to the client
//! they were given; what comes back is a [`Response`] whose body streams.
//! [`ReqwestClient`] is the default: reqwest over rustls, no proxy, a redirect
//! followed only within its origin, one connection pool shared by the whole
//! process, built on the first request. A host hands in its own client to send
//! those somewhere else (a gateway, a recorder, a test's fake); a test's fake
//! is one method.
//!
//! Two parts do not send through the client a host hands the others:
//!
//! - **WebFetch** sends through an `HttpClient` of its own, which checks every
//!   address it connects to (haest-tools' `WebFetchNet`; a host's client there
//!   gives that check up).
//! - **An MCP server's streamable HTTP** runs on rmcp's transport, which takes
//!   only a `reqwest::Client`: haest-mcp builds it here, with the same
//!   settings, but its requests never pass an `HttpClient`.
//!
//! This crate is also the one place haest builds a reqwest client and touches
//! TLS ([`build_reqwest`]).
//!
//! Through the facade it is `haest::http`; every crate that sends re-exports
//! it as `http` (`haest_providers::http`, `haest_tools::http`,
//! `haest_issues::http`), so an implementation needs no other crate:
//! [`async_trait`](macro@async_trait) and [`Bytes`] are re-exported here.
//!
//! # Examples
//!
//! A fake that answers every request from memory, and a caller reading the
//! answer, offline:
//!
//! ```
//! use haest_http::{HttpClient, HttpError, Request, Response, async_trait};
//! use serde_json::json;
//!
//! /// Answers every request with one JSON object.
//! struct Fake;
//!
//! #[async_trait]
//! impl HttpClient for Fake {
//!     async fn send(&self, req: Request) -> Result<Response, HttpError> {
//!         assert_eq!(req.header("authorization"), Some("Bearer t0k3n"));
//!         Ok(Response::new(200, "").with_json(&json!({"answer": 42})))
//!     }
//! }
//!
//! # #[tokio::main(flavor = "current_thread")]
//! # async fn main() -> Result<(), haest_http::BoxError> {
//! let http = Fake;
//! let req = Request::get("https://api.example.com/answer").with_header("Authorization", "Bearer t0k3n");
//! let res = http.send(req).await?;
//! assert!(res.is_success());
//! let body = res.body.bytes(1024 * 1024).await?;
//! let answer: serde_json::Value = serde_json::from_slice(&body)?;
//! assert_eq!(answer["answer"], 42);
//! # Ok(())
//! # }
//! ```
//!
//! A part that sends holds its client as an [`HttpClientRef`], and takes
//! `impl Into<HttpClientRef>`: the client itself, or an `Arc` of one the host
//! shares.
//!
//! The default sends for real; nothing is built until the first request:
//!
//! ```no_run
//! use haest_http::{HttpClient, Request, ReqwestClient};
//!
//! # #[tokio::main(flavor = "current_thread")]
//! # async fn main() -> Result<(), haest_http::HttpError> {
//! let res = ReqwestClient::new().send(Request::get("https://example.com/")).await?;
//! println!("{} ({} headers)", res.status, res.headers.len());
//! # Ok(())
//! # }
//! ```

#![cfg_attr(docsrs, feature(doc_cfg))]

mod client_ref;
mod error;
mod redact;
mod request;
mod reqwest_client;
mod response;

/// Shared by haest's crates, not part of the API: the Debug redaction their
/// option and tracker types use.
#[doc(hidden)]
pub mod __private {
    pub use crate::redact::{HeaderNames, REDACTED, UrlDebug, without_userinfo};
}

/// Marks an `impl HttpClient` so its `async fn` compiles; the macro the trait
/// is declared with.
pub use async_trait::async_trait;
/// A cheaply cloned byte buffer: a request's body, a response's chunks.
pub use bytes::Bytes;
/// The reqwest haest builds on, for [`ReqwestClient::configured`] and
/// [`build_reqwest`]: name its types from here and the versions always match.
///
/// Its types are part of haest-http's API: a breaking reqwest release (0.14
/// after 0.13) is a breaking release of haest-http and of haest.
pub use reqwest;

pub use client_ref::HttpClientRef;
pub use error::{BoxError, HttpError};
pub use request::{Method, ParseMethodError, Request};
pub use reqwest_client::{ReqwestClient, build_reqwest, reqwest_builder};
pub use response::{Body, Response};

/// PLUG POINT: how haest sends a request.
///
/// An implementation:
///
/// - returns `Ok` as soon as the status and headers are in, for **any**
///   status: the caller decides what a 404 or a 529 means.
/// - returns [`HttpError::Connect`] when no answer came and another try may
///   help (refused, reset, unresolved, timed out), [`HttpError::Request`]
///   when the request cannot be sent as given. Only the first kind is retried.
/// - ends the body with an `Err` chunk ([`HttpError::Body`]) if it breaks off.
/// - may be dropped mid-request: that is how a caller cancels (a model call
///   the human stopped, a fetch that timed out). Hold nothing that must
///   outlive the future.
///
/// [`ReqwestClient`] is the default; the crate docs have a fake. A part
/// that sends holds one as an [`HttpClientRef`].
#[async_trait]
pub trait HttpClient: Send + Sync {
    /// Send `req`. Returns once the status and headers are in; the body streams.
    ///
    /// # Errors
    ///
    /// An [`HttpError`] when no answer arrived; an error status is not one.
    async fn send(&self, req: Request) -> Result<Response, HttpError>;
}
