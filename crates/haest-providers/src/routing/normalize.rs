//! Tolerant reads of an untrusted routing blob (an env var, a stored row, a
//! request body): anything malformed is dropped, never thrown. A bad override
//! must not stop haest from answering -- it just routes as if that override
//! were absent.

use serde_json::Value;

use super::{
    ProjectRouting, ProviderRouting, RoutingEnv, RoutingOverride, RoutingPriority, RoutingRule,
    pattern_refusal, unique,
};

fn slug_list(v: Option<&Value>) -> Vec<String> {
    let Some(list) = v.and_then(Value::as_array) else {
        return Vec::new();
    };
    unique(
        list.iter()
            .filter_map(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string),
    )
}

fn rule(v: Option<&Value>) -> RoutingRule {
    RoutingRule {
        banned: slug_list(v.and_then(|r| r.get("banned"))),
        favorites: slug_list(v.and_then(|r| r.get("favorites"))),
    }
}

/// One scope from untrusted JSON:
/// `{"global": {"banned": [...], "favorites": [...]}, "overrides": [{"match": "...", "banned": [...], "favorites": [...]}]}`.
///
/// Never fails. Slugs are trimmed, blank ones dropped, repeats removed. An
/// override whose pattern [`pattern_refusal`] refuses, or whose pattern came
/// before, is skipped. Anything that is not the right shape reads as empty.
pub fn normalize_routing(raw: &Value) -> ProviderRouting {
    let Some(r) = raw.as_object() else {
        return ProviderRouting::default();
    };
    let mut overrides: Vec<RoutingOverride> = Vec::new();
    for o in r
        .get("overrides")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let pattern = o.get("match").and_then(Value::as_str).unwrap_or("").trim();
        if pattern_refusal(pattern).is_some() || overrides.iter().any(|x| x.pattern == pattern) {
            continue;
        }
        overrides.push(RoutingOverride {
            pattern: pattern.to_string(),
            rule: rule(Some(o)),
        });
    }
    ProviderRouting {
        global: rule(r.get("global")),
        overrides,
    }
}

/// A project scope from untrusted JSON: [`normalize_routing`]'s shape plus
/// `"priority"` (`"cost"`, `"speed"` or `"privacy"`; anything else is none).
pub fn normalize_project_routing(raw: &Value) -> ProjectRouting {
    let priority = match raw.get("priority").and_then(Value::as_str) {
        Some("cost") => Some(RoutingPriority::Cost),
        Some("speed") => Some(RoutingPriority::Speed),
        Some("privacy") => Some(RoutingPriority::Privacy),
        _ => None,
    };
    ProjectRouting {
        routing: normalize_routing(raw),
        priority,
    }
}

/// True when the scope would never change a request.
pub fn is_empty_routing(config: &ProjectRouting) -> bool {
    says_nothing(&config.routing) && config.priority.is_none()
}

fn says_nothing(r: &ProviderRouting) -> bool {
    r.global.banned.is_empty() && r.global.favorites.is_empty() && r.overrides.is_empty()
}

/// The routing scopes in a host's JSON (haest's `provider_routing` setting):
/// the fleet scope's `global` + `overrides`, and an optional `project` scope.
/// None when empty. Malformed JSON is an `Err` the caller reports, naming
/// where the JSON came from, and then routes as if unset -- loudly, never
/// fatally.
///
/// Valid JSON of the wrong shape is read tolerantly ([`normalize_routing`]):
/// what does not fit is dropped.
///
/// # Errors
///
/// A message when `raw` is not blank and not valid JSON.
pub fn parse_routing(raw: &str) -> Result<Option<RoutingEnv>, String> {
    if raw.trim().is_empty() {
        return Ok(None);
    }
    let parsed: Value =
        serde_json::from_str(raw).map_err(|e| format!("not valid JSON, ignoring it: {e}"))?;
    let fleet = normalize_routing(&parsed);
    let project = parsed
        .get("project")
        .filter(|p| !p.is_null())
        .map(normalize_project_routing)
        .filter(|p| !is_empty_routing(p));
    if says_nothing(&fleet) && project.is_none() {
        return Ok(None);
    }
    Ok(Some(RoutingEnv { fleet, project }))
}
