//! How a model reasons: the levels it takes and whether it can stop.

use serde::{Deserialize, Serialize};

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
