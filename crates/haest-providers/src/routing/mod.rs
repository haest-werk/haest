//! OPENROUTER PROVIDER ROUTING -- the config shapes, read from a host's JSON.
//!
//! Two SCOPES, each a default rule (banned + favorite providers) plus OVERRIDES
//! keyed by a model or family:
//!   - the FLEET scope, every haest conversation;
//!   - a PROJECT scope, which may also set a PRIORITY: cost, speed or privacy.
//!
//! At request time the OpenRouter provider turns the scopes plus the CURRENT
//! model into OpenRouter's request `provider` object (a conversation can switch
//! models, so it is resolved per request, never once at boot).
//!
//! Resolution:
//!   - BANS are a UNION of every matching layer in both scopes. A provider banned
//!     anywhere that applies stays banned.
//!   - FAVORITES come from the MOST SPECIFIC layer that names any: project model >
//!     project family (longest literal first) > project default > fleet model >
//!     fleet family > fleet default. A layer with no favorites inherits -- one that
//!     only bans must not silently wipe the favorites above it.
//!   - A favorite that is also banned is dropped: `ignore` wins.
//!   - PRIORITY is the project's alone.
//!
//! Wire mapping (<https://openrouter.ai/docs/features/provider-routing>):
//!   banned -> `ignore`; favorites -> `order` + `allow_fallbacks: true` (a
//!   preference, never a whitelist); cost -> `sort: "price"`; speed ->
//!   `sort: "throughput"`; privacy -> `zdr: true` + `data_collection: "deny"`.
//!
//! Provider ids are OpenRouter SLUGS (`deepinfra`), never display names.
//!
//! Start from [`parse_routing`]: a host's JSON (haest's `provider_routing`
//! setting) in, a [`RoutingScopes`] out, which goes into
//! [`RegistryOptions::with_routing`](crate::RegistryOptions::with_routing) or
//! [`CompatOptions::routing`](crate::CompatOptions::routing). A host may also
//! build the scopes in code; every field is public.
//!
//! ```
//! use haest_providers::routing::{RoutingPriority, parse_routing};
//!
//! # fn main() -> Result<(), haest_core::error::BoxError> {
//! let routing = parse_routing(r#"{
//!     "global": {"banned": ["novita"], "favorites": ["deepinfra"]},
//!     "overrides": [{"match": "z-ai/glm-*", "favorites": ["fireworks"]}],
//!     "project": {"priority": "privacy"}
//! }"#)?
//! .ok_or("blank routing")?;
//!
//! // For z-ai/glm-5.3 the override's favorites win (the most specific layer),
//! // novita stays banned and only zero-data-retention providers answer.
//! assert_eq!(routing.fleet.global.banned, ["novita"]);
//! assert_eq!(routing.fleet.overrides[0].pattern, "z-ai/glm-*");
//! let priority = routing.project.and_then(|p| p.priority);
//! assert_eq!(priority, Some(RoutingPriority::Privacy));
//! # Ok(())
//! # }
//! ```

mod normalize;
mod patterns;
#[cfg(test)]
mod tests;

use std::{cmp::Reverse, iter};

use serde::Serialize;

pub use normalize::parse_routing;
use patterns::{pattern_matches, specificity};

/// One layer's say: providers to skip and providers to try first.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RoutingRule {
    /// Provider slugs OpenRouter must skip.
    pub banned: Vec<String>,
    /// Provider slugs to try first, in order. Empty = inherit.
    pub favorites: Vec<String>,
}

/// A rule for the models one pattern matches.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoutingOverride {
    /// An exact OpenRouter model id (`z-ai/glm-4.6`), a glob family whose `*`
    /// matches any run of characters (`z-ai/glm-*`), or an author prefix
    /// ending in `/` (`z-ai/`). It is matched against the model id without an
    /// `openrouter:` prefix or a `:variant` suffix.
    pub pattern: String,
    /// What applies to the models it matches.
    pub rule: RoutingRule,
}

/// One scope: its default rule and its overrides.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProviderRouting {
    /// The scope's default rule, for every model.
    pub global: RoutingRule,
    /// Rules for the models each pattern matches; the most specific wins for favorites.
    pub overrides: Vec<RoutingOverride>,
}

haest_core::word_enum! {
/// What a project scope asks OpenRouter to favour: the `priority` word.
/// More may come in a minor release, as OpenRouter adds ways to sort.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum RoutingPriority ("routing priority") {
    /// The cheapest provider first: `sort: "price"`.
    Cost = "cost",
    /// The fastest provider first: `sort: "throughput"`.
    Speed = "speed",
    /// Only providers that keep no data and do not collect it:
    /// `zdr: true`, `data_collection: "deny"`.
    Privacy = "privacy",
}
}

/// A project's scope: the same shape as the fleet's, plus a priority.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProjectRouting {
    /// The project's default rule and overrides.
    pub routing: ProviderRouting,
    /// What to favour; None = OpenRouter's own order.
    pub priority: Option<RoutingPriority>,
}

/// The routing scopes haest routes by: the fleet scope, plus its project's
/// scope when it has one.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RoutingScopes {
    /// The fleet scope: every conversation.
    pub fleet: ProviderRouting,
    /// The project scope, which wins over the fleet's favorites; None = none.
    pub project: Option<ProjectRouting>,
}

/// OpenRouter's request `provider` object. A None field is left out of the JSON.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub(crate) struct ProviderPrefs {
    /// Provider slugs to skip: every ban that applies, in either scope.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ignore: Option<Vec<String>>,
    /// Provider slugs to try first, in order: the most specific favorites, banned ones dropped.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub order: Option<Vec<String>>,
    /// Set with `order`, always true: the order is a preference, never a whitelist.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub allow_fallbacks: Option<bool>,
    /// `"price"` or `"throughput"`, from the project's priority.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sort: Option<&'static str>,
    /// True for the privacy priority: zero-data-retention providers only.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub zdr: Option<bool>,
    /// `"deny"` for the privacy priority.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data_collection: Option<&'static str>,
}

/// One scope's rules that apply to `model`: its matching overrides, most
/// specific first, then its default.
fn rules_for<'a>(scope: &'a ProviderRouting, model: &str) -> impl Iterator<Item = &'a RoutingRule> {
    let mut hits: Vec<&RoutingOverride> = scope
        .overrides
        .iter()
        .filter(|o| pattern_matches(&o.pattern, model))
        .collect();
    hits.sort_by_key(|o| Reverse(specificity(&o.pattern)));
    hits.into_iter()
        .map(|o| &o.rule)
        .chain(iter::once(&scope.global))
}

pub(crate) fn unique(list: impl IntoIterator<Item = String>) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for s in list {
        if !out.contains(&s) {
            out.push(s);
        }
    }
    out
}

impl RoutingScopes {
    /// The `provider` object for `model`'s request by the rules in the
    /// [module docs](self): bans from every matching layer, favorites from the
    /// most specific layer that has any, the priority from the project. None
    /// when no scope says anything for it.
    ///
    /// `model` may be an OpenRouter id (`z-ai/glm-4.6`) or a haest spec
    /// (`openrouter:z-ai/glm-4.6:free`): the prefix and a `:variant` are ignored.
    pub(crate) fn provider_prefs(&self, model: &str) -> Option<ProviderPrefs> {
        let project = self.project.as_ref();
        let rules: Vec<&RoutingRule> = project
            .into_iter()
            .flat_map(|p| rules_for(&p.routing, model))
            .chain(rules_for(&self.fleet, model))
            .collect();
        let banned = unique(rules.iter().flat_map(|r| r.banned.iter().cloned()));
        let favorites = rules.iter().find(|r| !r.favorites.is_empty());
        let order: Vec<String> = unique(favorites.map(|r| r.favorites.clone()).unwrap_or_default())
            .into_iter()
            .filter(|slug| !banned.contains(slug))
            .collect();
        let mut prefs = ProviderPrefs::default();
        if !banned.is_empty() {
            prefs.ignore = Some(banned);
        }
        if !order.is_empty() {
            prefs.order = Some(order);
            prefs.allow_fallbacks = Some(true);
        }
        match project.and_then(|p| p.priority) {
            Some(RoutingPriority::Cost) => prefs.sort = Some("price"),
            Some(RoutingPriority::Speed) => prefs.sort = Some("throughput"),
            Some(RoutingPriority::Privacy) => {
                prefs.zdr = Some(true);
                prefs.data_collection = Some("deny");
            }
            None => {}
        }
        (prefs != ProviderPrefs::default()).then_some(prefs)
    }
}
