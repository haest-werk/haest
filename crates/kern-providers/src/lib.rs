//! Model API adapters for kern.
//!
//! | Wire dialect | Provider | Used by |
//! |---|---|---|
//! | Anthropic Messages | [`AnthropicProvider`] | `anthropic`, a `type: "anthropic"` entry of providers.json |
//! | OpenAI Chat Completions | [`CompatProvider`] | `openrouter`, `paperinstruments`, any OpenAI-compatible server |
//! | OpenAI Responses | [`ResponsesProvider`] | `openai` |
//!
//! Every adapter streams server-sent events over a [`Transport`] (PLUG POINT:
//! the default is [`ReqwestTransport`]; a host may route every model call
//! through its own gateway) and ends with exactly one `ProviderEvent::Done`,
//! also on failure. [`ProviderRegistry`] turns a `provider:model` spec into a
//! live provider; [`Catalog`] answers what a cataloged model accepts.

pub mod anthropic;
pub mod catalog;
pub mod compat;
pub mod compat_effort;
mod driver;
mod http;
pub mod openai_media;
pub mod registry;
mod reqwest_transport;
pub mod responses;
pub mod routing;
pub mod sse;
#[cfg(any(test, feature = "testing"))]
pub mod testing;
pub mod tool_args;
pub mod transport;
pub mod web_search;

#[cfg(test)]
mod driver_tests;
#[cfg(test)]
mod media_tests;

pub use anthropic::{AnthropicOptions, AnthropicProvider};
pub use catalog::Catalog;
pub use compat::{CompatFlavor, CompatOptions, CompatProvider};
pub use http::RetryPolicy;
pub use registry::{
    CustomProviderConfig, ProviderConfigError, ProviderRegistry, RegistryOptions,
    load_custom_providers,
};
pub use reqwest_transport::ReqwestTransport;
pub use responses::{ResponsesOptions, ResponsesProvider};
pub use routing::RoutingEnv;
pub use transport::{ByteStream, HttpRequest, HttpResponse, Transport, TransportError};
