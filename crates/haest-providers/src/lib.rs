//! Model API adapters for haest: the providers that turn one model call into
//! an HTTP request and stream the answer back.
//!
//! Each adapter implements haest-core's [`Provider`]. It takes a neutral
//! [`ProviderRequest`] (model, system prompt, messages, tools, effort, a
//! cancellation token), POSTs it in its API's wire format, and streams
//! [`ProviderEvent`]s as the server sends them: text and thinking deltas, the
//! start of each tool call, and last, exactly once, `Done` with the whole
//! [`AssistantMessage`].
//!
//! | Wire dialect | Provider | Used by |
//! |---|---|---|
//! | Anthropic Messages | [`AnthropicProvider`] | `anthropic`, a `"type": "anthropic"` entry of providers.json |
//! | OpenAI Chat Completions | [`CompatProvider`] | `openrouter`, `paperinstruments`, a `"type": "openai"` entry: any OpenAI-compatible server |
//! | OpenAI Responses | [`ResponsesProvider`] | `openai` |
//!
//! # Where it fits
//!
//! Through the facade this crate is `haest::providers` (the `providers`
//! feature, on by default); the request and event types are haest-core's
//! (`haest::core`). Depend on `haest-providers` directly when you want the
//! model calls without the rest of haest: your own loop, a one-shot
//! completion, a model picker over the [`Catalog`].
//!
//! The entry points:
//!
//! - [`ProviderRegistry`]: a `provider:model` spec in, a live provider out. It
//!   is haest-core's [`ModelResolver`], what the agent loop asks for a model.
//! - [`AnthropicProvider`], [`CompatProvider`], [`ResponsesProvider`]: build
//!   one yourself to set every option.
//! - [`Catalog`]: the models haest knows, and what each accepts.
//! - [`http::HttpClient`]: how every request travels (PLUG POINT). The
//!   default is [`http::ReqwestClient`], the process's shared client.
//! - [`routing`]: OpenRouter provider routing, resolved per request.
//!
//! # Picking a model
//!
//! A model is named by a spec, `provider:model`:
//! `openrouter:anthropic/claude-sonnet-5.5`, `anthropic:claude-opus-5-5`,
//! `openai:gpt-6.1-sol`, or `local:qwen3-coder` for a providers.json entry
//! named `local`. Everything after the first `:` is the provider's own model
//! id, sent as is. [`Catalog::builtin`] lists the models haest knows; a spec
//! outside it works too, with nothing known about it.
//!
//! The built-in providers ask the host's [`KeySource`] for a key and an
//! optional base URL, by these names:
//!
//! | Provider | Key | Base URL (optional) |
//! |---|---|---|
//! | `anthropic` | `ANTHROPIC_API_KEY` | `ANTHROPIC_BASE_URL` |
//! | `openai` | `OPENAI_API_KEY` | `OPENAI_BASE_URL` |
//! | `openrouter` | `OPENROUTER_API_KEY` | `OPENROUTER_BASE_URL` |
//! | `paperinstruments` | `PAPERINSTRUMENTS_API_KEY` | `PAPERINSTRUMENTS_BASE_URL` |
//!
//! This crate never reads the process environment: the host answers those
//! names from wherever it keeps keys (a `HashMap<String, String>`, a closure,
//! a vault). A missing key is an error when its provider is first resolved,
//! not before.
//!
//! # Plugging in your own HTTP client
//!
//! Every provider sends one POST with a JSON body and reads the answer as a
//! byte stream of server-sent events. Implement [`http::HttpClient`] to send
//! it somewhere else: a gateway over a unix socket, a recorder, a fake for
//! tests. The provider keeps the rest: headers, retries, SSE parsing, error
//! messages.
//!
//! This example answers every call with one canned Chat Completions stream,
//! resolves an OpenRouter spec over it and reads the answer, offline:
//!
//! ```
//! use std::{collections::HashMap, sync::Arc};
//!
//! use futures::StreamExt;
//! use haest_core::{Message, ProviderEvent, ProviderRequest, StopReason};
//! use haest_providers::{
//!     CustomProviders, ProviderRegistry, RegistryOptions,
//!     http::{HttpClient, HttpError, Request, Response, async_trait},
//! };
//! use tokio_util::sync::CancellationToken;
//!
//! /// Answers every request with the same stream. A real client would send
//! /// `req` over its own connection.
//! struct Canned;
//!
//! #[async_trait]
//! impl HttpClient for Canned {
//!     async fn send(&self, req: Request) -> Result<Response, HttpError> {
//!         assert_eq!(req.url, "https://openrouter.ai/api/v1/chat/completions");
//!         assert_eq!(req.header("authorization"), Some("Bearer sk-test"));
//!         let sse = concat!(
//!             "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"Hello!\"}}]}\n\n",
//!             "data: {\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
//!             "data: [DONE]\n\n",
//!         );
//!         Ok(Response::new(200, sse).with_header("content-type", "text/event-stream"))
//!     }
//! }
//!
//! #[tokio::main(flavor = "current_thread")]
//! async fn main() {
//!     // The host answers key names; here from a map.
//!     let keys = HashMap::from([("OPENROUTER_API_KEY".to_string(), "sk-test".to_string())]);
//!     let registry = ProviderRegistry::new(
//!         Arc::new(keys),
//!         CustomProviders::new(),
//!         RegistryOptions { http: Arc::new(Canned), ..RegistryOptions::default() },
//!     );
//!
//!     let resolved = registry.resolve_spec("openrouter:anthropic/claude-sonnet-5.5").unwrap();
//!     assert_eq!(resolved.model, "anthropic/claude-sonnet-5.5");
//!
//!     let request = ProviderRequest {
//!         model: resolved.model.clone(),
//!         system: "You are terse.".into(),
//!         messages: vec![Message::user("Say hello.")],
//!         tools: Vec::new(),
//!         cancel: CancellationToken::new(),
//!         effort: None,
//!         max_tokens: None,
//!         web_search: None,
//!     };
//!     let mut stream = resolved.provider.stream(request);
//!     while let Some(event) = stream.next().await {
//!         match event {
//!             ProviderEvent::TextDelta(text) => print!("{text}"),
//!             ProviderEvent::Done(message) => {
//!                 assert_eq!(message.text(), "Hello!");
//!                 assert_eq!(message.stop_reason, StopReason::EndTurn);
//!             }
//!             _ => {}
//!         }
//!     }
//! }
//! ```
//!
//! Everything an implementation names is in [`http`] (the haest-http crate,
//! re-exported): the trait, its types, the `async_trait` macro it is declared
//! with, and `Bytes`. A host needs no other crate for it.
//!
//! # providers.json
//!
//! More providers come from a JSON object, one entry per provider name. The
//! haest CLI reads it from `<config dir>/providers.json` with
//! [`load_custom_providers`]; a host may build the [`CustomProviders`] map
//! itself. [`CustomProviderConfig`] describes every key:
//!
//! ```json
//! {
//!   "local": {
//!     "type": "openai",
//!     "baseURL": "http://localhost:1234/v1",
//!     "apiKeyEnv": "LOCAL_LLM_KEY",
//!     "reasoningEffort": { "efforts": ["low", "medium", "high"], "mandatory": false },
//!     "pricing": { "qwen3-coder": { "input": 0, "cachedInput": 0, "output": 0 } }
//!   },
//!   "gateway": {
//!     "type": "anthropic",
//!     "baseURL": "https://llm-gateway.example.com",
//!     "apiKeyEnv": "GATEWAY_API_KEY",
//!     "headers": { "X-Team": "platform" }
//!   }
//! }
//! ```
//!
//! With that file, `local:qwen3-coder` and `gateway:claude-opus-5-5` resolve
//! like the built-ins. An entry is checked when its provider is first
//! resolved, so a bad entry fails only its own specs. An entry named like a
//! built-in (`openrouter`) replaces it.
//!
//! # Failure, retries and cancellation
//!
//! A provider's stream never errors and always ends with exactly one
//! [`ProviderEvent::Done`]. A request the server refused, a body that broke
//! off or an error event in the stream ends it with a message whose
//! `stop_reason` is [`StopReason::Error`] and whose `error_message` says why
//! (for a refusal, `"<status> <the API's own message>"`). The text and
//! thinking that arrived are kept; no tool call is, since one may be cut off.
//!
//! A request is retried the way the official SDKs do ([`RetryPolicy`]); once
//! the answer has started streaming, nothing is retried. Cancelling the
//! request's [`ProviderRequest::cancel`] token stops the wait, the retries and
//! the stream, and `Done` arrives with [`StopReason::Aborted`].
//!
//! # Features
//!
//! - `testing`: the `testing` module, fakes for tests in other crates: an
//!   [`http::HttpClient`] that answers from a script and records every
//!   request, and builders for SSE bodies in each dialect.
//!
//! [`Provider`]: haest_core::provider::Provider
//! [`ProviderRequest`]: haest_core::provider::ProviderRequest
//! [`ProviderRequest::cancel`]: haest_core::provider::ProviderRequest::cancel
//! [`ProviderEvent`]: haest_core::provider::ProviderEvent
//! [`ProviderEvent::Done`]: haest_core::provider::ProviderEvent::Done
//! [`ModelResolver`]: haest_core::provider::ModelResolver
//! [`AssistantMessage`]: haest_core::messages::AssistantMessage
//! [`StopReason::Error`]: haest_core::messages::StopReason::Error
//! [`StopReason::Aborted`]: haest_core::messages::StopReason::Aborted
//! [`KeySource`]: haest_core::KeySource

#![cfg_attr(docsrs, feature(doc_cfg))]
#![warn(missing_docs)]

pub mod anthropic;
pub mod catalog;
pub mod compat;
mod driver;
mod openai_media;
pub mod registry;
pub mod responses;
pub mod routing;
mod send;
mod sse;
#[cfg(any(test, feature = "testing"))]
pub mod testing;
mod web_search;

/// How every model request travels: the haest-http crate, everything a host
/// needs to bring its own [`HttpClient`](http::HttpClient).
#[doc(inline)]
pub use haest_http as http;

#[cfg(test)]
mod driver_tests;
#[cfg(test)]
mod media_tests;

pub use anthropic::{AnthropicOptions, AnthropicProvider};
pub use catalog::Catalog;
pub use compat::{CompatFlavor, CompatOptions, CompatProvider};
pub use registry::{
    CustomProviderConfig, CustomProviders, EntryProblem, ProviderConfigError, ProviderRegistry,
    ProvidersFileError, RegistryOptions, load_custom_providers,
};
pub use responses::{ResponsesOptions, ResponsesProvider};
pub use routing::RoutingEnv;
pub use send::RetryPolicy;

/// A tool result's text as every dialect sends it: never empty.
pub(crate) fn result_text(msg: &haest_core::messages::ToolResultMessage) -> &str {
    if msg.content.is_empty() {
        "(empty)"
    } else {
        &msg.content
    }
}
