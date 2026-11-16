//! haest's one HTTP seam: every request a haest part sends goes through an
//! [`HttpClient`].
//!
//! The model calls (haest-providers), WebFetch and GenerateImage (haest-tools)
//! and the GitHub tracker (haest-issues) build a [`Request`] and hand it to the
//! client they were given; what comes back is a [`Response`] whose body
//! streams. [`ReqwestClient`] is the default: reqwest over rustls, no proxy,
//! one connection pool shared by the whole process, built on the first
//! request. A host hands in its own client to send everything somewhere else
//! (a gateway, a recorder, a test's fake); a test's fake is one method.
//!
//! This crate is also the one place haest builds a reqwest client and touches
//! TLS ([`build_reqwest`]): the MCP client, which needs a reqwest client of
//! its own, gets it from here too.
//!
//! Through the facade it is `haest::http`; haest-providers re-exports it as
//! `haest_providers::http`, so an implementation needs no other crate:
//! [`async_trait`] and [`Bytes`] are re-exported here.
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
//!         Ok(Response::json(200, &json!({"answer": 42})))
//!     }
//! }
//!
//! # #[tokio::main(flavor = "current_thread")]
//! # async fn main() -> Result<(), HttpError> {
//! let http = Fake;
//! let req = Request::get("https://api.example.com/answer").with_header("Authorization", "Bearer t0k3n");
//! let res = http.send(req).await?;
//! assert!(res.is_success());
//! let body = res.body.bytes(1024 * 1024).await?;
//! let answer: serde_json::Value = serde_json::from_slice(&body).unwrap();
//! assert_eq!(answer["answer"], 42);
//! # Ok(())
//! # }
//! ```
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
#![warn(missing_docs)]

mod error;
mod request;
mod reqwest_client;
mod response;

/// Marks an `impl HttpClient` so its `async fn` compiles; the macro the trait
/// is declared with.
pub use async_trait::async_trait;
/// A cheaply cloned byte buffer: a request's body, a response's chunks.
pub use bytes::Bytes;
/// The reqwest haest builds on, for [`ReqwestClient::configured`] and
/// [`build_reqwest`]: name its types from here and the versions always match.
pub use reqwest;

pub use error::{BoxError, HttpError};
pub use request::{Method, Request};
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
/// [`ReqwestClient`] is the default; the crate docs have a fake.
#[async_trait]
pub trait HttpClient: Send + Sync {
    /// Send `req`. Returns once the status and headers are in; the body streams.
    ///
    /// # Errors
    ///
    /// An [`HttpError`] when no answer arrived; an error status is not one.
    async fn send(&self, req: Request) -> Result<Response, HttpError>;
}
