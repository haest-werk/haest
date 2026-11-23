//! The shapes of a model catalog row: who makes a model and which one it is,
//! what it takes as input, how it reasons, what it costs, what its prompt
//! cache can do and how big its context is. The rows themselves live
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

mod cache_facts;
mod pricing;
mod reasoning;

use std::collections::HashSet;

use serde::{Deserialize, Serialize};

pub use cache_facts::CacheFacts;
pub use pricing::ModelPricing;
pub use reasoning::{OpenRouterEffort, Reasoning};

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
    /// The spec's whole catalog row; None = unknown. The other methods
    /// answer from it unless the host answers them itself, so a host with a
    /// catalog implements this one.
    fn model_info(&self, _spec: &str) -> Option<ModelInfo> {
        None
    }

    /// What the spec takes as input; None = unknown, media sent untouched.
    /// Media a model does not take is replaced by a note before the call.
    fn input_modalities(&self, spec: &str) -> Option<HashSet<InputModality>> {
        self.model_info(spec).map(|m| m.input.into_iter().collect())
    }

    /// The spec's reasoning row (which efforts it takes); None = unknown,
    /// effort sent as asked.
    fn reasoning(&self, spec: &str) -> Option<Reasoning> {
        self.model_info(spec)?.reasoning
    }

    /// The spec's price; None = unpriced, and nothing that weighs a cost
    /// (keeping the cache warm, compacting before it expires) runs.
    fn pricing(&self, spec: &str) -> Option<ModelPricing> {
        self.model_info(spec)?.pricing
    }

    /// What the spec's prompt cache can do; None = unknown, a request's
    /// cache breakpoints sent as planned.
    fn cache_facts(&self, spec: &str) -> Option<CacheFacts> {
        self.model_info(spec)?.cache
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

/// One model as a catalog lists it: its spec, who makes it and which one it
/// is, what it takes, and what is known about its reasoning, price and
/// prompt cache.
///
/// Built with [`new`](Self::new) and the `with_*` setters, so a part it gains
/// later breaks no host; its fields read as they are.
///
/// `family`, `tier` and `generation` are written in the catalog, never read
/// out of the spec: model ids follow no shared pattern (`claude-opus-5-5`,
/// `gpt-6.1-sol-pro`, `qwen3.8-max-0902`).
///
/// ```
/// use haest_core::{
///     InputModality,
///     catalog::{ModelInfo, ModelPricing},
/// };
///
/// let row = ModelInfo::new("openrouter:example/model-1-flash", 200_000)
///     .with_label("Example Model 1 Flash")
///     .with_family("example")
///     .with_tier("flash")
///     .with_generation(1.0)
///     .with_input([InputModality::Text, InputModality::Image])
///     .with_pricing(ModelPricing::new(3.0, 0.3, 15.0));
/// assert_eq!(row.label, "Example Model 1 Flash");
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
    /// Who makes the model, as OpenRouter names its author (`anthropic`,
    /// `openai`, `google`, `x-ai`); absent = unknown.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub family: Option<String>,
    /// Which model of its family and generation it is: the words its label
    /// adds, lowercase and joined by `-` (`opus`, `flash`, `sol-pro`);
    /// absent = the label adds none, or unknown.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tier: Option<String>,
    /// The version its label carries, as a number (`5.5` for Claude Opus 5.5,
    /// `4` for Gemini 4 Argon); absent = unknown. It compares within one
    /// family only, and a float has no `5.10` after `5.9`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub generation: Option<f64>,
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
    /// What its prompt cache can do; absent = unknown: haest sends a
    /// request's cache breakpoints as planned and keeps nothing warm.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache: Option<CacheFacts>,
}

impl ModelInfo {
    /// The model `spec` serving `context_window` tokens: labelled with its
    /// spec, taking text only, its family, tier, generation, reasoning and
    /// cache unknown, and unpriced.
    pub fn new(spec: impl Into<String>, context_window: u64) -> Self {
        let spec = spec.into();
        Self {
            label: spec.clone(),
            spec,
            family: None,
            tier: None,
            generation: None,
            input: vec![InputModality::Text],
            context_window,
            reasoning: None,
            pricing: None,
            cache: None,
        }
    }

    /// The name a picker shows.
    #[must_use]
    pub fn with_label(mut self, label: impl Into<String>) -> Self {
        self.label = label.into();
        self
    }

    /// Who makes the model ([`family`](Self::family)).
    #[must_use]
    pub fn with_family(mut self, family: impl Into<String>) -> Self {
        self.family = Some(family.into());
        self
    }

    /// Which model of its family it is ([`tier`](Self::tier)).
    #[must_use]
    pub fn with_tier(mut self, tier: impl Into<String>) -> Self {
        self.tier = Some(tier.into());
        self
    }

    /// The version its label carries ([`generation`](Self::generation)).
    #[must_use]
    pub fn with_generation(mut self, generation: f64) -> Self {
        self.generation = Some(generation);
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

    /// What its prompt cache can do.
    #[must_use]
    pub fn with_cache(mut self, cache: CacheFacts) -> Self {
        self.cache = Some(cache);
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

#[cfg(test)]
mod tests {
    use super::*;

    /// A host that knows its rows and nothing else.
    struct Rows;
    impl ModelFacts for Rows {
        fn model_info(&self, spec: &str) -> Option<ModelInfo> {
            (spec == "local:m").then(|| {
                ModelInfo::new(spec, 8_000)
                    .with_input([InputModality::Text, InputModality::Image])
                    .with_reasoning(Reasoning::new([OpenRouterEffort::High]).with_mandatory(true))
                    .with_pricing(ModelPricing::new(1.0, 0.1, 2.0))
                    .with_cache(CacheFacts::new().with_min_cacheable_tokens(1_024))
            })
        }
    }

    #[test]
    fn a_host_with_rows_answers_every_fact_from_them() {
        let inputs = Rows.input_modalities("local:m").unwrap();
        assert!(inputs.contains(&InputModality::Image));
        assert!(Rows.reasoning("local:m").unwrap().mandatory);
        assert_eq!(Rows.pricing("local:m").map(|p| p.input), Some(1.0));
        let cache = Rows.cache_facts("local:m").unwrap();
        assert_eq!(cache.min_cacheable_tokens, Some(1_024));
        assert_eq!(Rows.input_modalities("local:other"), None);
        assert_eq!(Rows.reasoning("local:other"), None);
        assert_eq!(Rows.pricing("local:other"), None);
        assert_eq!(Rows.cache_facts("local:other"), None);
    }
}
