//! The shapes of a model catalog row: what a model takes as input, how it
//! reasons, what it costs and how big its context is. The rows themselves live
//! with the providers (`haest-providers`' `Catalog`, bundled from a JSON file);
//! a host may bring its own rows in the same shape.
//!
//! The loop never reads a catalog directly. It asks the host through
//! [`AgentHooks`](crate::AgentHooks) (`input_modalities`, `reasoning`), and the
//! answers come from these rows: the loop drops input a model cannot take
//! ([`crate::media`]) and fits the reasoning level, and a provider adapter books
//! a cost from [`ModelPricing`].
//!
//! # Examples
//!
//! A row as the catalog file writes it (camelCase keys):
//!
//! ```
//! use haest_core::catalog::{InputModality, ModelInfo};
//!
//! let row: ModelInfo = serde_json::from_str(r#"{
//!     "spec": "openrouter:example/model-1",
//!     "label": "Example Model 1",
//!     "input": ["text", "image"],
//!     "contextWindow": 200000,
//!     "reasoning": { "mandatory": false, "efforts": ["low", "high"] },
//!     "pricing": { "input": 3.0, "cachedInput": 0.3, "output": 15.0 }
//! }"#).unwrap();
//!
//! assert!(row.input.contains(&InputModality::Image));
//! assert_eq!(row.pricing.unwrap().cache_write, None);
//! ```

use serde::{Deserialize, Serialize};

/// A kind of input a model accepts, in OpenRouter's names (`file` is a PDF).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum InputModality {
    /// Text.
    Text,
    /// An image.
    Image,
    /// A document: a PDF.
    File,
    /// Audio.
    Audio,
    /// Video.
    Video,
}

/// A reasoning level in OpenRouter's vocabulary (`reasoning.supported_efforts`).
///
/// Wider than haest's own [`Effort`](crate::Effort): `none` and `minimal` have
/// no haest name; each of haest's levels but `off` has one of the same name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OpenRouterEffort {
    /// Reasoning off.
    None,
    /// Less than `low`; haest never asks for it.
    Minimal,
    /// `low`.
    Low,
    /// `medium`.
    Medium,
    /// `high`.
    High,
    /// `xhigh`.
    Xhigh,
    /// `max`.
    Max,
}

/// How a model reasons: whether it can stop, and the levels it accepts.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Reasoning {
    /// The model cannot turn reasoning off: `enabled: false` is a 400, not a cheaper turn.
    pub mandatory: bool,
    /// The levels the model accepts, in any order.
    pub efforts: Vec<OpenRouterEffort>,
}

/// A model's price in USD per 1M tokens, for a provider whose API returns no
/// cost of its own ([`crate::pricing::book_priced_cost`]).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelPricing {
    /// Uncached input tokens.
    pub input: f64,
    /// Input tokens read from the provider's prompt cache.
    pub cached_input: f64,
    /// Output tokens.
    pub output: f64,
    /// Input tokens written to the prompt cache; absent = billed as `input`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_write: Option<f64>,
}

/// One model as a catalog lists it: its spec, what it takes, and what is known
/// about its reasoning and price.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelInfo {
    /// `provider:model`, exactly what a model spec takes.
    pub spec: String,
    /// The name a picker shows.
    pub label: String,
    /// What the model accepts as input.
    pub input: Vec<InputModality>,
    /// Tokens the model actually serves (the best endpoint's cap, not the advertised one).
    pub context_window: u64,
    /// Absent = unknown: haest sends the effort as asked.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<Reasoning>,
    /// Absent = unpriced: haest books the provider's own cost, or none. Never guessed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pricing: Option<ModelPricing>,
}

/// A labelled group of models, as a picker shows them (by vendor, say).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelGroup {
    /// The group's heading.
    pub label: String,
    /// The group's models, in the order a picker lists them.
    pub models: Vec<ModelInfo>,
}
