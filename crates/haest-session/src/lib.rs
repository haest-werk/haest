//! A haest conversation another program can watch and drive while it runs.
//!
//! The pieces, each usable alone:
//!
//! - [`Hub`]: one stream. Every frame published into it gets the next seq,
//!   goes into a replay ring (haest-wire's `Ring`) and out to every
//!   [`Subscription`]. Publishing never waits: a subscriber that falls behind
//!   gets one `gap` frame, and the turn goes on.
//! - [`start`]: the session driver over a [`Driven`] (a bare `Agent`, or
//!   haest-runtime's `Runtime` with its `session` feature). It owns the
//!   conversation on its own task and runs one turn at a time. Input that
//!   arrives during a turn is steered into it (it lands at the next step);
//!   input that arrives while the driver is busy otherwise (a change being
//!   made, a hold) waits and runs as a turn of its own, through the turn gate
//!   (a person's input starts its count over, another program's counts
//!   against it). Model, effort and clear wait for the running turn to end.
//! - [`SessionHandle`]: the running session, driven through its own methods,
//!   and a [`Backend`]: the one seam a transport needs (haest-transport
//!   serves it over stdio, a Unix socket or a broker link). Its frames are
//!   [`Frame`]s: the wire's envelope around a [`Body`], the conversation's
//!   events or the wire's own frames.
//! - [`SessionPrompter`] (feature `approvals`): approval asks answered by a
//!   peer, for a conversation with no terminal.
//!
//! Through the facade this crate is `haest::session` (feature `session`, not
//! default). Nothing here reads a file, a variable or a socket: a transport
//! does that, and only when a host attaches one.
//!
//! # A host's order
//!
//! 1. Make the [`Hub`] first, with the stream's id: whatever the build says
//!    (a notice through the host's UI) is already in the stream.
//! 2. Build the conversation, its prompter a [`SessionPrompter`] over the hub
//!    when nobody sits at a terminal.
//! 3. [`start`] the session over it; subscribe the host's own display, attach
//!    transports.
//! 4. Drive it through the handle; [`SessionHandle::close`] at the end stops
//!    what the conversation started and saves it.
//!
//! # Examples
//!
//! Input sent mid-turn joins the running turn at its next step. The tool
//! here waits until it is let go, so the input arrives while it runs.
//!
//! ```
//! use haest_core::{
//!     Agent, AgentDeps, AgentOptions, JsonMap, Message, ShutWorkspace, Tool, ToolContext,
//!     ToolError, ToolOutput, Tools,
//!     testing::{ScriptedProvider, text_answer, tool_answer},
//! };
//! use haest_session::{Hub, Input, start};
//! use tokio::sync::watch;
//!
//! /// A tool that answers once it is let go.
//! struct Wait(watch::Receiver<bool>);
//!
//! #[async_trait::async_trait]
//! impl Tool for Wait {
//!     fn name(&self) -> &str { "Wait" }
//!     fn description(&self) -> String { "Waits.".into() }
//!     fn parameters(&self) -> serde_json::Value { serde_json::json!({"type": "object"}) }
//!     fn kind(&self) -> &str { "wait" }
//!     async fn execute(&self, _: JsonMap, _: &ToolContext) -> Result<ToolOutput, ToolError> {
//!         let _ = self.0.clone().wait_for(|go| *go).await;
//!         Ok(ToolOutput::text("waited"))
//!     }
//! }
//!
//! # #[tokio::main(flavor = "current_thread")]
//! # async fn main() -> Result<(), haest_core::BoxError> {
//! let (go, gate) = watch::channel(false);
//! let model = ScriptedProvider::new([
//!     tool_answer(&[("c1", "Wait", serde_json::json!({}))]),
//!     text_answer("done, and noted"),
//! ]);
//! let tools = Tools::new().with(Wait(gate));
//! let deps = AgentDeps::new(model.resolver(), tools, "", "/", ShutWorkspace);
//! let session = start(Agent::new(deps, AgentOptions::new("scripted:m")), Hub::new("s1"));
//!
//! let first = tokio::spawn({
//!     let session = session.clone();
//!     async move { session.turn(Input::human("wait, then answer")).await }
//! });
//! // While the tool runs: a second input, then let the tool go.
//! while model.requests().is_empty() { tokio::task::yield_now().await; }
//! let second = tokio::spawn({
//!     let session = session.clone();
//!     async move { session.turn(Input::human("also note this")).await }
//! });
//! tokio::time::sleep(std::time::Duration::from_millis(20)).await;
//! go.send_replace(true);
//!
//! // One turn took both: the second input is a user message after the tool result.
//! first.await??;
//! second.await??;
//! let history = session.history().await?.messages;
//! assert!(matches!(&history[3], Message::User(u) if u.content == "also note this"));
//! assert_eq!(model.requests().len(), 2);
//! session.close().await;
//! # Ok(())
//! # }
//! ```

#![cfg_attr(docsrs, feature(doc_cfg))]
#![warn(missing_docs)]

mod backend;
mod body;
mod driven;
mod driver;
mod handle;
mod hub;
#[cfg(feature = "approvals")]
mod prompter;
mod subscription;
#[cfg(test)]
mod tests;

pub use backend::{Backend, Control, History, Input, Meta, SessionError};
pub use body::{Body, Frame};
pub use driven::{Driven, DrivenStatus};
pub use handle::{Hold, PendingTurn, SessionHandle, start};
pub use hub::{Hub, HubOptions};
#[cfg(feature = "approvals")]
#[cfg_attr(docsrs, doc(cfg(feature = "approvals")))]
pub use prompter::SessionPrompter;
pub use subscription::Subscription;

/// A boxed future borrowing for `'a`: what a [`SessionHandle::with`] closure returns.
pub type BoxFuture<'a, T> = std::pin::Pin<Box<dyn std::future::Future<Output = T> + Send + 'a>>;
