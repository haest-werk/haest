//! OPENROUTER PROVIDER ROUTING -- the config shapes and their ONE resolver.
//!
//! Two SCOPES, each a default rule (banned + favorite providers) plus OVERRIDES
//! keyed by a model or family (`patterns`):
//!   - the FLEET scope, every haest conversation;
//!   - a PROJECT scope, which may also set a PRIORITY: cost, speed or privacy.
//!
//! At request time haest turns the scopes plus the CURRENT model into OpenRouter's
//! request `provider` object (a conversation can switch models, so it is resolved
//! per request, never once at boot).
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
//! setting) in, a [`RoutingEnv`] out, which goes into
//! [`RegistryOptions::routing`](crate::RegistryOptions::routing) or
//! [`CompatOptions::routing`](crate::CompatOptions::routing). The OpenRouter
//! provider then calls [`provider_routing_field`] for each request's model.
//!
//! ```
//! use haest_providers::routing::{parse_routing, provider_routing_field};
//!
//! let routing = parse_routing(r#"{
//!     "global": {"banned": ["novita"], "favorites": ["deepinfra"]},
//!     "overrides": [{"match": "z-ai/glm-*", "favorites": ["fireworks"]}],
//!     "project": {"priority": "privacy"}
//! }"#)
//! .unwrap();
//!
//! let glm = provider_routing_field(routing.as_ref(), "z-ai/glm-5.3").unwrap();
//! assert_eq!(glm.ignore, Some(vec!["novita".to_string()]));
//! assert_eq!(glm.order, Some(vec!["fireworks".to_string()])); // the override's, the most specific
//! assert_eq!(glm.zdr, Some(true));
//!
//! let other = provider_routing_field(routing.as_ref(), "openai/gpt-6.1-sol").unwrap();
//! assert_eq!(other.order, Some(vec!["deepinfra".to_string()]));
//! ```

mod normalize;
mod patterns;
#[cfg(test)]
mod tests;

use serde::Serialize;

pub use normalize::{
    is_empty_routing, normalize_project_routing, normalize_routing, parse_routing,
};
pub use patterns::{MatchKind, match_kind, pattern_matches, pattern_refusal, specificity};

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
    /// An exact model id (`z-ai/glm-4.6`), a glob family (`z-ai/glm-*`) or an
    /// author prefix (`z-ai/`); see [`pattern_matches`].
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

/// What a project scope asks OpenRouter to favour.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RoutingPriority {
    /// The cheapest provider first: `sort: "price"`.
    Cost,
    /// The fastest provider first: `sort: "throughput"`.
    Speed,
    /// Only providers that keep no data and do not collect it:
    /// `zdr: true`, `data_collection: "deny"`.
    Privacy,
}

/// A project's scope: the same shape as the fleet's, plus a priority.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProjectRouting {
    /// The project's default rule and overrides.
    pub routing: ProviderRouting,
    /// What to favour; None = OpenRouter's own order.
    pub priority: Option<RoutingPriority>,
}

/// What haest routes by: the fleet scope, plus its project's scope when it has one.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RoutingEnv {
    /// The fleet scope: every conversation.
    pub fleet: ProviderRouting,
    /// The project scope, which wins over the fleet's favorites; None = none.
    pub project: Option<ProjectRouting>,
}

/// OpenRouter's request `provider` object. A None field is left out of the JSON.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct ProviderPrefs {
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

/// Which scope a layer of routing belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LayerScope {
    /// The project's scope.
    Project,
    /// The fleet's scope.
    Fleet,
}

/// How specific a layer of routing is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LayerLevel {
    /// An override whose pattern is one model id.
    Model,
    /// An override whose pattern is a family or an author.
    Family,
    /// The scope's `global` rule.
    Default,
}

/// The routing for one model, with where it came from, to show the human why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedRouting {
    /// The `provider` object to send; None when nothing applies.
    pub provider: Option<ProviderPrefs>,
    /// The layer the favorites came from; None when there are none.
    pub favorites_from: Option<(LayerScope, LayerLevel)>,
    /// Override patterns that matched, most specific first, project scope first.
    pub matched: Vec<String>,
}

struct Layer<'a> {
    rule: &'a RoutingRule,
    scope: LayerScope,
    level: LayerLevel,
    pattern: Option<&'a str>,
}

/// One scope's matching layers, most specific first, its default last.
fn layers_of<'a>(
    scope: LayerScope,
    config: Option<&'a ProviderRouting>,
    model: &str,
) -> Vec<Layer<'a>> {
    let Some(config) = config else {
        return Vec::new();
    };
    let mut hits: Vec<&RoutingOverride> = config
        .overrides
        .iter()
        .filter(|o| pattern_matches(&o.pattern, model))
        .collect();
    hits.sort_by_key(|o| std::cmp::Reverse(specificity(&o.pattern)));
    let level = |p: &str| match match_kind(p) {
        MatchKind::Model => LayerLevel::Model,
        MatchKind::Family => LayerLevel::Family,
    };
    hits.into_iter()
        .map(|o| Layer {
            rule: &o.rule,
            scope,
            level: level(&o.pattern),
            pattern: Some(&o.pattern),
        })
        .chain(std::iter::once(Layer {
            rule: &config.global,
            scope,
            level: LayerLevel::Default,
            pattern: None,
        }))
        .collect()
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

/// The routing for `model` from the fleet scope `config` and the project scope
/// `project`, by the rules in the [module docs](self): bans from every
/// matching layer, favorites from the most specific layer that has any, the
/// priority from the project.
///
/// `model` may be an OpenRouter id (`z-ai/glm-4.6`) or a haest spec
/// (`openrouter:z-ai/glm-4.6:free`): the prefix and a `:variant` are ignored.
pub fn resolve_provider_routing(
    config: Option<&ProviderRouting>,
    model: &str,
    project: Option<&ProjectRouting>,
) -> ResolvedRouting {
    let mut layers = layers_of(LayerScope::Project, project.map(|p| &p.routing), model);
    layers.extend(layers_of(LayerScope::Fleet, config, model));
    let banned = unique(layers.iter().flat_map(|l| l.rule.banned.iter().cloned()));
    let source = layers.iter().find(|l| !l.rule.favorites.is_empty());
    let order: Vec<String> = unique(source.map(|l| l.rule.favorites.clone()).unwrap_or_default())
        .into_iter()
        .filter(|slug| !banned.contains(slug))
        .collect();
    let mut prefs = ProviderPrefs::default();
    if !banned.is_empty() {
        prefs.ignore = Some(banned);
    }
    let favorites_from = match source {
        Some(l) if !order.is_empty() => Some((l.scope, l.level)),
        _ => None,
    };
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
    ResolvedRouting {
        provider: (prefs != ProviderPrefs::default()).then_some(prefs),
        favorites_from,
        matched: layers
            .iter()
            .filter_map(|l| l.pattern.map(str::to_string))
            .collect(),
    }
}

/// `provider` for the OpenRouter request body of `model`, or None when no scope says anything for it.
pub fn provider_routing_field(routing: Option<&RoutingEnv>, model: &str) -> Option<ProviderPrefs> {
    let routing = routing?;
    resolve_provider_routing(Some(&routing.fleet), model, routing.project.as_ref()).provider
}
