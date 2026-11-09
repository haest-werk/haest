//! Tolerant reads of an untrusted routing blob (an env var, a stored row, a
//! request body): anything malformed is dropped, never thrown. A bad override
//! must not stop kern from answering -- it just routes as if that override
//! were absent.

use std::collections::HashMap;

use serde_json::Value;

use super::{
    ProjectRouting, ProviderRouting, RoutingEnv, RoutingOverride, RoutingPriority, RoutingRule,
    pattern_refusal, unique,
};

/// The environment variable a host puts the routing scopes in, as JSON:
/// the fleet scope's `global` + `overrides`, and an optional `project` scope.
pub const ROUTING_ENV: &str = "KERN_PROVIDER_ROUTING";

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
    let r = &config.routing;
    r.global.banned.is_empty()
        && r.global.favorites.is_empty()
        && r.overrides.is_empty()
        && config.priority.is_none()
}

/// The scopes in [`ROUTING_ENV`], or None when unset or empty. Malformed JSON is
/// an `Err` the caller reports and then routes as if unset -- loudly, never fatally.
pub fn routing_from_env(env: &HashMap<String, String>) -> Result<Option<RoutingEnv>, String> {
    let Some(raw) = env.get(ROUTING_ENV).filter(|v| !v.is_empty()) else {
        return Ok(None);
    };
    let parsed: Value = serde_json::from_str(raw)
        .map_err(|e| format!("{ROUTING_ENV} is not valid JSON, ignoring it: {e}"))?;
    let fleet = normalize_routing(&parsed);
    let project = parsed
        .get("project")
        .filter(|p| !p.is_null())
        .map(normalize_project_routing)
        .filter(|p| !is_empty_routing(p));
    let fleet_empty = is_empty_routing(&ProjectRouting {
        routing: fleet.clone(),
        priority: None,
    });
    if fleet_empty && project.is_none() {
        return Ok(None);
    }
    Ok(Some(RoutingEnv { fleet, project }))
}
