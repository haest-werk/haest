//! The shapes of a model catalog row: what a model takes as input, how it
//! reasons, what it costs and how big its context is. The rows themselves live
//! with the providers (`haest-providers`' `Catalog`, bundled from a JSON file);
//! a host may bring its own rows in the same shape.
//!
//! The loop never reads a catalog directly. It asks the host's [`ModelFacts`]
//! ([`SharedDeps::facts`](crate::SharedDeps::facts)), and the answers come from
//! these rows: the loop drops input a model cannot take ([`crate::media`]),
//! fits the reasoning level and caps the turn, and a provider adapter books a
//! cost from [`ModelPricing`].
//!
//! # Examples
//!
//! A row as the catalog file writes it (camelCase keys):
//!
//! ```
//! use haest_core::{InputModality, catalog::ModelInfo};
//!
//! let row: ModelInfo = serde_json::from_str(r#"{
//!     "spec": "openrouter:example/model-1",
//!     "label": "Example Model 1",
//!     "input": ["text", "image"],
//!     "contextWindow": 200000,
//!     "reasoning": { "mandatory": false, "efforts": ["low", "high"] },
//!     "pricing": { "input": 3.0, "cachedInput": 0.3, "output": 15.0 }
//! }"#)?;
//!
//! assert!(row.input.contains(&InputModality::Image));
//! assert_eq!(row.pricing.map(|p| p.cache_write), Some(None));
//! # Ok::<(), serde_json::Error>(())
//! ```

use std::collections::HashSet;

use serde::{Deserialize, Serialize};

/// PLUG POINT: what the host knows about a model spec
/// ([`Agent::model_spec`](crate::Agent::model_spec)), set on
/// [`SharedDeps::facts`](crate::SharedDeps::facts). Asked again before every
/// model call, so the answers may follow the model. Every method defaults to
/// "unknown"; [`NoFacts`] knows nothing.
///
/// Facts only: what the host hears about (a message, withheld media, a
/// fitted effort) goes to [`AgentHooks`](crate::AgentHooks), and its caps
/// per model are a [`TurnCapsSource`](crate::caps::TurnCapsSource).
pub trait ModelFacts: Send + Sync {
    /// What the spec takes as input; None = unknown, media sent untouched.
    /// Media a model does not take is replaced by a note before the call.
    fn input_modalities(&self, _spec: &str) -> Option<HashSet<InputModality>> {
        None
    }

    /// The spec's reasoning row (which efforts it takes); None = unknown,
    /// effort sent as asked.
    fn reasoning(&self, _spec: &str) -> Option<Reasoning> {
        None
    }
}

crate::plug_ref! {
    /// A [`ModelFacts`], shared: what [`SharedDeps::facts`](crate::SharedDeps::facts) holds.
    pub struct ModelFactsRef(dyn ModelFacts);
}

/// Facts about no model: every answer "unknown". [`AgentDeps::new`](crate::AgentDeps::new)'s default.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct NoFacts;

impl ModelFacts for NoFacts {}

crate::word_enum! {
    /// A kind of input a model accepts, in OpenRouter's names (`file` is a PDF).
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
    #[non_exhaustive]
    pub enum InputModality ("input modality") {
        /// Text.
        Text = "text",
        /// An image.
        Image = "image",
        /// A document: a PDF.
        File = "file",
        /// Audio.
        Audio = "audio",
        /// Video.
        Video = "video",
    }
}

crate::word_enum! {
/// A reasoning level in OpenRouter's vocabulary (`reasoning.supported_efforts`).
///
/// Wider than haest's own [`Effort`](crate::Effort): `none` and `minimal` have
/// no haest name; each of haest's levels but `off` has one of the same name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[non_exhaustive]
pub enum OpenRouterEffort ("reasoning effort") {
    /// Reasoning off.
    None = "none",
    /// Less than `low`; haest never asks for it.
    Minimal = "minimal",
    /// `low`.
    Low = "low",
    /// `medium`.
    Medium = "medium",
    /// `high`.
    High = "high",
    /// `xhigh`.
    Xhigh = "xhigh",
    /// `max`.
    Max = "max",
}
}

/// How a model reasons: whether it can stop, and the levels it accepts.
///
/// Built with [`new`](Self::new) and [`with_mandatory`](Self::with_mandatory),
/// so a part it gains later breaks no host; its fields read as they are.
///
/// ```
/// use haest_core::catalog::{OpenRouterEffort, Reasoning};
///
/// let r = Reasoning::new([OpenRouterEffort::Low, OpenRouterEffort::High]).with_mandatory(true);
/// assert!(r.mandatory);
/// assert_eq!(r.efforts.len(), 2);
/// ```
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct Reasoning {
    /// The model cannot turn reasoning off: `enabled: false` is a 400, not a cheaper turn.
    pub mandatory: bool,
    /// The levels the model accepts, in any order.
    pub efforts: Vec<OpenRouterEffort>,
}

impl Reasoning {
    /// A model that accepts `efforts` and may turn reasoning off.
    pub fn new(efforts: impl IntoIterator<Item = OpenRouterEffort>) -> Self {
        Self {
            mandatory: false,
            efforts: efforts.into_iter().collect(),
        }
    }

    /// Whether the model cannot turn reasoning off.
    #[must_use]
    pub fn with_mandatory(mut self, mandatory: bool) -> Self {
        self.mandatory = mandatory;
        self
    }
}

/// A model's price in USD per 1M tokens, for a provider whose API returns no
/// cost of its own ([`book_priced_cost`](crate::provider::book_priced_cost)).
///
/// Built with [`new`](Self::new) and [`with_cache_write`](Self::with_cache_write),
/// so a price it gains later (for reasoning, for a search) breaks no host; its
/// fields read as they are.
///
/// ```
/// use haest_core::catalog::ModelPricing;
///
/// // $3 in, $0.30 from the cache, $15 out; a cache write costs $3.75.
/// let price = ModelPricing::new(3.0, 0.3, 15.0).with_cache_write(3.75);
/// assert_eq!(price.cache_write, Some(3.75));
/// ```
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
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

impl ModelPricing {
    /// `input`, `cached_input` and `output` USD per 1M tokens, in that
    /// order; a cache write is billed as input.
    pub fn new(input: f64, cached_input: f64, output: f64) -> Self {
        Self {
            input,
            cached_input,
            output,
            cache_write: None,
        }
    }

    /// Cache writes at `usd` per 1M tokens, not as input.
    #[must_use]
    pub fn with_cache_write(mut self, usd: f64) -> Self {
        self.cache_write = Some(usd);
        self
    }
}

/// One model as a catalog lists it: its spec, what it takes, and what is known
/// about its reasoning and price.
///
/// Built with [`new`](Self::new) and the `with_*` setters, so a part it gains
/// later breaks no host; its fields read as they are.
///
/// ```
/// use haest_core::{
///     InputModality,
///     catalog::{ModelInfo, ModelPricing},
/// };
///
/// let row = ModelInfo::new("openrouter:example/model-1", 200_000)
///     .with_label("Example Model 1")
///     .with_input([InputModality::Text, InputModality::Image])
///     .with_pricing(ModelPricing::new(3.0, 0.3, 15.0));
/// assert_eq!(row.label, "Example Model 1");
/// assert_eq!(row.reasoning, None, "unknown: the effort is sent as asked");
/// ```
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
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

impl ModelInfo {
    /// The model `spec` serving `context_window` tokens: labelled with its
    /// spec, taking text only, its reasoning unknown and unpriced.
    pub fn new(spec: impl Into<String>, context_window: u64) -> Self {
        let spec = spec.into();
        Self {
            label: spec.clone(),
            spec,
            input: vec![InputModality::Text],
            context_window,
            reasoning: None,
            pricing: None,
        }
    }

    /// The name a picker shows.
    #[must_use]
    pub fn with_label(mut self, label: impl Into<String>) -> Self {
        self.label = label.into();
        self
    }

    /// What the model accepts as input, in place of text only.
    #[must_use]
    pub fn with_input(mut self, input: impl IntoIterator<Item = InputModality>) -> Self {
        self.input = input.into_iter().collect();
        self
    }

    /// How the model reasons.
    #[must_use]
    pub fn with_reasoning(mut self, reasoning: Reasoning) -> Self {
        self.reasoning = Some(reasoning);
        self
    }

    /// What the model costs.
    #[must_use]
    pub fn with_pricing(mut self, pricing: ModelPricing) -> Self {
        self.pricing = Some(pricing);
        self
    }
}

/// A labelled group of models, as a picker shows them (by vendor, say).
///
/// Built with [`new`](Self::new), so a part it gains later breaks no host.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct ModelGroup {
    /// The group's heading.
    pub label: String,
    /// The group's models, in the order a picker lists them.
    pub models: Vec<ModelInfo>,
}

impl ModelGroup {
    /// The group `label` of `models`, in the order a picker lists them.
    pub fn new(label: impl Into<String>, models: impl IntoIterator<Item = ModelInfo>) -> Self {
        Self {
            label: label.into(),
            models: models.into_iter().collect(),
        }
    }
}
