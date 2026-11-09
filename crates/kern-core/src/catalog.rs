//! The shapes of a model catalog row. The rows themselves live with the
//! providers (`kern-providers`); a host may bring its own.

use serde::{Deserialize, Serialize};

/// What a model takes as input. OpenRouter's names: `file` = a PDF.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum InputModality {
    Text,
    Image,
    File,
    Audio,
    Video,
}

/// OpenRouter's effort vocabulary (`reasoning.supported_efforts`), wider than kern's own levels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OpenRouterEffort {
    None,
    Minimal,
    Low,
    Medium,
    High,
    Xhigh,
    Max,
}

/// A model's reasoning row.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Reasoning {
    /// The model cannot turn reasoning off: `enabled: false` is a 400, not a cheaper turn.
    pub mandatory: bool,
    pub efforts: Vec<OpenRouterEffort>,
}

/// USD per 1M tokens, for a provider whose API returns no cost of its own.
/// `cache_write` absent = billed as input.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelPricing {
    pub input: f64,
    pub cached_input: f64,
    pub output: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_write: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelInfo {
    /// `provider:model`, exactly what a model spec takes.
    pub spec: String,
    pub label: String,
    pub input: Vec<InputModality>,
    /// Tokens the model actually serves (the best endpoint's cap, not the advertised one).
    pub context_window: u64,
    /// Absent = unknown: kern sends the effort as asked.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<Reasoning>,
    /// Absent = unpriced: kern books the provider's own cost, or none. Never guessed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pricing: Option<ModelPricing>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelGroup {
    pub label: String,
    pub models: Vec<ModelInfo>,
}
