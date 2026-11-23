//! [`CatalogFacts`]: what haest knows about a model, from the built-in
//! catalog with providers.json's own facts over it.

use std::collections::HashSet;

use haest_core::catalog::{
    CacheFacts, InputModality, ModelFacts, ModelInfo, ModelPricing, Reasoning,
};

use crate::{Catalog, CustomProviderConfig, CustomProviders};

/// The [`ModelFacts`] of the built-in [`Catalog`] with a providers.json over
/// it: a model's row is the catalog's, and an entry's `pricing` and `cache`
/// for one of its models stand in place of that row's. A model only an
/// entry lists has no row ([`model_info`](ModelFacts::model_info) None) but
/// is priced, and its cache known, from the entry. An entry that cannot be
/// read adds nothing (resolving its provider says why).
///
/// ```
/// use haest_core::catalog::ModelFacts;
/// use haest_providers::{CatalogFacts, CustomProviders};
///
/// # fn main() -> Result<(), serde_json::Error> {
/// let file: CustomProviders = serde_json::from_str(r#"{"gateway": {"type": "anthropic",
///     "baseURL": "https://llm-gateway.example.com",
///     "cache": {"claude-opus-5-5": {"minCacheableTokens": 512, "ttl1h": false}}}}"#)?;
/// let facts = CatalogFacts::new(file);
///
/// // The gateway serves no hour-long cache entries, whatever Anthropic's do.
/// let gateway = facts.cache_facts("gateway:claude-opus-5-5").map(|c| c.ttl_1h);
/// assert_eq!(gateway, Some(false));
/// let direct = facts.cache_facts("anthropic:claude-opus-5-5").map(|c| c.ttl_1h);
/// assert_eq!(direct, Some(true));
/// assert!(facts.model_info("gateway:claude-opus-5-5").is_none(), "not a catalog row");
/// # Ok(())
/// # }
/// ```
#[derive(Debug, Clone, Default)]
pub struct CatalogFacts {
    custom: CustomProviders,
}

impl CatalogFacts {
    /// The built-in catalog with `custom`'s facts over it.
    pub fn new(custom: CustomProviders) -> Self {
        Self { custom }
    }

    /// What `spec`'s providers.json entry says about its model through
    /// `fact`; None without an entry, an entry that cannot be read, or one
    /// that says nothing of it.
    fn entry<T>(
        &self,
        spec: &str,
        fact: impl FnOnce(&mut CustomProviderConfig, &str) -> Option<T>,
    ) -> Option<T> {
        let (provider, model) = spec.split_once(':')?;
        let mut entry = self.custom.get(provider)?.ok()?;
        fact(&mut entry, model)
    }
}

/// Answers from the built-in catalog, but for a providers.json entry's
/// `pricing` and `cache`.
impl ModelFacts for CatalogFacts {
    fn model_info(&self, spec: &str) -> Option<ModelInfo> {
        let mut row = Catalog::builtin().model(spec)?.clone();
        row.pricing = self.pricing(spec);
        row.cache = self.cache_facts(spec);
        Some(row)
    }

    // Answered from the catalog without cloning the whole row.
    fn input_modalities(&self, spec: &str) -> Option<HashSet<InputModality>> {
        Catalog::builtin().input_modalities(spec)
    }

    fn reasoning(&self, spec: &str) -> Option<Reasoning> {
        Catalog::builtin().reasoning(spec).cloned()
    }

    fn pricing(&self, spec: &str) -> Option<ModelPricing> {
        self.entry(spec, |e, model| e.pricing.remove(model))
            .or_else(|| Catalog::builtin().pricing(spec).cloned())
    }

    fn cache_facts(&self, spec: &str) -> Option<CacheFacts> {
        let row = || Catalog::builtin().model(spec)?.cache.clone();
        self.entry(spec, |e, model| e.cache.remove(model))
            .or_else(row)
    }
}
