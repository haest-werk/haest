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
//! The default ([`HttpClientRef::default`], the shared [`ReqwestClient`])
//! sends for real; nothing is built until the first request:
//!
//! ```no_run
//! use haest_http::{HttpClientRef, Request};
//!
//! # #[tokio::main(flavor = "current_thread")]
//! # async fn main() -> Result<(), haest_http::HttpError> {
//! let res = HttpClientRef::default().send(Request::get("https://example.com/")).await?;
//! println!("{} ({} headers)", res.status, res.headers.len());
//! # Ok(())
//! # }
//! ```
//!
//! # Features
//!
//! - `reqwest` (default): [`ReqwestClient`], [`build_reqwest`] and the
//!   [`reqwest`](mod@reqwest) re-export. Off, this crate is the trait and its
//!   types only, it builds no reqwest, rustls or ring, and the default client
//!   refuses every request ([`HttpError::Request`]) until a host hands in its
//!   own: what a host with its own HTTP stack (a gateway, a recorder) wants.
//!   haest-providers and haest-issues forward it as their own `reqwest`
//!   feature, and the facade as `haest/reqwest`.
//! - `http`: conversions to and from the [`http`](https://docs.rs/http/1)
//!   crate's types, for a host whose stack speaks them (an axum router, a
//!   hyper client): `http::Request<Bytes>: TryFrom<Request>`,
//!   `Response: From<http::Response<B>>` for any `http_body::Body` `B`, and
//!   `http::Method: From<Method>`. With `reqwest` on it adds no crate. A
//!   host on the facade turns it on with a direct `haest-http` dependency
//!   (`features = ["http"]`), and imports the facade's module under another
//!   name (`use haest::http as haest_http;`) beside the `http` crate.
//! - `testing`: `testing::Server`, an HTTP server on 127.0.0.1 that writes
//!   down each request and answers it as a test says. For
//!   `dev-dependencies` only: like every `testing` module and feature in
//!   haest, outside semver.
//!
//! ```
//! # #[cfg(feature = "http")]
//! # fn main() -> Result<(), haest_http::HttpError> {
//! use haest_http::{Bytes, Request, Response};
//!
//! // A gateway that answers from its own router: haest's request in, its answer back.
//! let req = Request::get("https://gateway.internal/v1/models").with_header("x-team", "a");
//! let req: http::Request<Bytes> = req.try_into()?;
//! assert_eq!(req.uri().path(), "/v1/models");
//! let answer = http::Response::new(String::from("[]"));
//! let res = Response::from(answer);
//! assert!(res.is_success());
//! # Ok(())
//! # }
//! # #[cfg(not(feature = "http"))]
//! # fn main() {}
//! ```

#![cfg_attr(docsrs, feature(doc_cfg))]

mod client_ref;
mod endpoint;
mod error;
#[cfg(feature = "http")]
mod http_crate;
mod redact;
mod request;
#[cfg(feature = "reqwest")]
mod reqwest_client;
mod response;
#[cfg(feature = "testing")]
#[cfg_attr(docsrs, doc(cfg(feature = "testing")))]
pub mod testing;

/// Shared by haest's crates, not part of the API: the Debug redaction their
/// option and tracker types use, a URL as an error message shows it, the
/// scrubbing of a reqwest error's URL, putting a path under a configured
/// base URL, and where a configured URL's authority is. haest-issues,
/// versioned on its own, never uses it (CONTRIBUTING, "One path per item").
#[doc(hidden)]
pub mod __private {
    pub use crate::endpoint::{authority, join_path};
    pub use crate::redact::{HeaderNames, REDACTED, UrlDebug, shown_url};
    #[cfg(feature = "reqwest")]
    pub use crate::reqwest_client::without_credentials;
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
#[cfg(feature = "reqwest")]
#[cfg_attr(docsrs, doc(cfg(feature = "reqwest")))]
pub use reqwest;

pub use client_ref::HttpClientRef;
pub use endpoint::{EndpointUrlError, check_endpoint_url};
pub use error::{BoxError, HttpError};
pub use redact::redacted_url;
pub use request::{Method, ParseMethodError, Request};
#[cfg(feature = "reqwest")]
#[cfg_attr(docsrs, doc(cfg(feature = "reqwest")))]
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
