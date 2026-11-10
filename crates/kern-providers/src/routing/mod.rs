//! OPENROUTER PROVIDER ROUTING -- the config shapes and their ONE resolver.
//!
//! Two SCOPES, each a default rule (banned + favorite providers) plus OVERRIDES
//! keyed by a model or family (`patterns`):
//!   - the FLEET scope, every kern conversation;
//!   - a PROJECT scope, which may also set a PRIORITY: cost, speed or privacy.
//!
//! At request time kern turns the scopes plus the CURRENT model into OpenRouter's
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

mod normalize;
mod patterns;
#[cfg(test)]
mod tests;

use serde::Serialize;

pub use normalize::{
    ROUTING_ENV, is_empty_routing, normalize_project_routing, normalize_routing, parse_routing,
};
pub use patterns::{MatchKind, match_kind, pattern_matches, pattern_refusal, specificity};

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RoutingRule {
    /// Provider slugs OpenRouter must skip.
    pub banned: Vec<String>,
    /// Provider slugs to try first, in order. Empty = inherit.
    pub favorites: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoutingOverride {
    pub pattern: String,
    pub rule: RoutingRule,
}

/// One scope: its default rule and its overrides.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProviderRouting {
    pub global: RoutingRule,
    pub overrides: Vec<RoutingOverride>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RoutingPriority {
    Cost,
    Speed,
    Privacy,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProjectRouting {
    pub routing: ProviderRouting,
    pub priority: Option<RoutingPriority>,
}

/// What kern routes by: the fleet scope, plus its project's scope when it has one.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RoutingEnv {
    pub fleet: ProviderRouting,
    pub project: Option<ProjectRouting>,
}

/// OpenRouter's request `provider` object.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct ProviderPrefs {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ignore: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub order: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub allow_fallbacks: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sort: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub zdr: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data_collection: Option<&'static str>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LayerScope {
    Project,
    Fleet,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LayerLevel {
    Model,
    Family,
    Default,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedRouting {
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
