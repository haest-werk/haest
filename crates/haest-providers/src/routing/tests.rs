use serde_json::json;

use super::{
    normalize::{normalize_project_routing, normalize_routing},
    patterns::{is_override_key, pattern_matches},
    *,
};

fn rule(banned: &[&str], favorites: &[&str]) -> RoutingRule {
    RoutingRule {
        banned: banned.iter().map(|s| s.to_string()).collect(),
        favorites: favorites.iter().map(|s| s.to_string()).collect(),
    }
}

fn ov(pattern: &str, banned: &[&str], favorites: &[&str]) -> RoutingOverride {
    RoutingOverride {
        pattern: pattern.into(),
        rule: rule(banned, favorites),
    }
}

fn cfg(global: RoutingRule, overrides: Vec<RoutingOverride>) -> ProviderRouting {
    ProviderRouting { global, overrides }
}

fn proj(global: RoutingRule, overrides: Vec<RoutingOverride>) -> ProjectRouting {
    ProjectRouting {
        routing: cfg(global, overrides),
        priority: None,
    }
}

fn sorted(mut v: Vec<String>) -> Vec<String> {
    v.sort();
    v
}

fn strings(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| s.to_string()).collect()
}

/// What the scopes send for `model`: the fleet scope (None = empty) and the project scope.
fn resolve(
    fleet: Option<&ProviderRouting>,
    model: &str,
    project: Option<&ProjectRouting>,
) -> Option<ProviderPrefs> {
    let scopes = RoutingScopes {
        fleet: fleet.cloned().unwrap_or_default(),
        project: project.cloned(),
    };
    scopes.provider_prefs(model)
}

fn order(r: &Option<ProviderPrefs>) -> Option<Vec<String>> {
    r.as_ref().and_then(|p| p.order.clone())
}

// -- fleet scope

#[test]
fn no_provider_object_for_an_empty_or_absent_config() {
    assert_eq!(resolve(None, "z-ai/glm-4.6", None), None);
    let empty = ProviderRouting::default();
    assert_eq!(resolve(Some(&empty), "z-ai/glm-4.6", None), None);
}

#[test]
fn default_bans_map_to_ignore_and_default_favorites_to_order_with_fallbacks_on() {
    let c = cfg(rule(&["novita"], &["deepinfra", "fireworks"]), vec![]);
    let r = resolve(Some(&c), "openrouter:z-ai/glm-4.6", None);
    assert_eq!(
        serde_json::to_value(r).unwrap(),
        json!({"ignore": ["novita"], "order": ["deepinfra", "fireworks"], "allow_fallbacks": true})
    );
}

#[test]
fn bans_union_across_the_default_and_every_matching_override() {
    let c = cfg(
        rule(&["novita"], &[]),
        vec![
            ov("z-ai/", &["chutes"], &[]),
            ov("z-ai/glm-*", &["baseten"], &["novita"]),
            ov("z-ai/glm-4.6", &["chutes"], &[]),
            ov("openai/", &["azure"], &[]),
        ],
    );
    let r = resolve(Some(&c), "z-ai/glm-4.6", None);
    let p = r.unwrap();
    assert_eq!(
        sorted(p.ignore.unwrap()),
        strings(&["baseten", "chutes", "novita"])
    );
    assert_eq!(p.order, None);
}

#[test]
fn favorites_come_from_the_exact_model_over_a_family_over_the_default() {
    let base = cfg(
        rule(&[], &["g"]),
        vec![
            ov("z-ai/", &[], &["author"]),
            ov("z-ai/glm-*", &[], &["family"]),
            ov("z-ai/glm-4.6", &[], &["model"]),
        ],
    );
    let at = |m: &str| resolve(Some(&base), m, None);
    assert_eq!(order(&at("z-ai/glm-4.6")), Some(strings(&["model"])));
    assert_eq!(order(&at("z-ai/glm-5")), Some(strings(&["family"])));
    assert_eq!(order(&at("z-ai/other")), Some(strings(&["author"])));
    assert_eq!(order(&at("openai/gpt-5")), Some(strings(&["g"])));
}

#[test]
fn an_override_with_no_favorites_inherits_the_next_layer_up() {
    let c = cfg(
        rule(&[], &["g"]),
        vec![
            ov("z-ai/glm-*", &[], &["family"]),
            ov("z-ai/glm-4.6", &["x"], &[]),
        ],
    );
    let r = resolve(Some(&c), "z-ai/glm-4.6", None);
    assert_eq!(
        serde_json::to_value(&r).unwrap(),
        json!({"ignore": ["x"], "order": ["family"], "allow_fallbacks": true})
    );
}

#[test]
fn a_favorite_a_matching_override_bans_is_dropped() {
    let c = cfg(rule(&[], &["a", "b"]), vec![ov("z-ai/", &["a"], &[])]);
    let r = resolve(Some(&c), "z-ai/glm-4.6", None);
    assert_eq!(
        serde_json::to_value(&r).unwrap(),
        json!({"ignore": ["a"], "order": ["b"], "allow_fallbacks": true})
    );
}

#[test]
fn matching_uses_the_bare_model_id() {
    let c = cfg(
        RoutingRule::default(),
        vec![ov("z-ai/glm-4.6", &["x"], &[])],
    );
    let r = resolve(Some(&c), "openrouter:z-ai/glm-4.6:nitro", None);
    assert_eq!(r.unwrap().ignore, Some(strings(&["x"])));
}

// -- project scope over fleet

fn fleet() -> ProviderRouting {
    cfg(
        rule(&["f-ban"], &["f-default"]),
        vec![ov("z-ai/glm-4.6", &["f-model-ban"], &["f-model"])],
    )
}

#[test]
fn a_project_default_favorite_beats_even_a_fleet_exact_model_favorite() {
    let p = proj(rule(&[], &["p-default"]), vec![]);
    let r = resolve(Some(&fleet()), "z-ai/glm-4.6", Some(&p));
    assert_eq!(order(&r), Some(strings(&["p-default"])));
}

#[test]
fn within_the_project_model_beats_family_beats_default() {
    let p = proj(
        rule(&[], &["p-default"]),
        vec![
            ov("z-ai/glm-*", &[], &["p-family"]),
            ov("z-ai/glm-4.6", &[], &["p-model"]),
        ],
    );
    let f = fleet();
    let at = |m: &str| resolve(Some(&f), m, Some(&p));
    assert_eq!(order(&at("z-ai/glm-4.6")), Some(strings(&["p-model"])));
    assert_eq!(order(&at("z-ai/glm-5")), Some(strings(&["p-family"])));
    assert_eq!(order(&at("qwen/x")), Some(strings(&["p-default"])));
}

#[test]
fn a_project_with_no_favorites_falls_through_to_the_fleet_layers() {
    let p = proj(rule(&["p-ban"], &[]), vec![]);
    let f = fleet();
    assert_eq!(
        order(&resolve(Some(&f), "z-ai/glm-4.6", Some(&p))),
        Some(strings(&["f-model"]))
    );
    assert_eq!(
        order(&resolve(Some(&f), "qwen/x", Some(&p))),
        Some(strings(&["f-default"]))
    );
}

#[test]
fn bans_union_across_both_scopes_and_a_fleet_ban_removes_a_project_favorite() {
    let p = proj(
        rule(&["p-ban"], &["f-ban", "ok"]),
        vec![ov("z-ai/", &["p-family-ban"], &[])],
    );
    let r = resolve(Some(&fleet()), "z-ai/glm-4.6", Some(&p));
    let prefs = r.unwrap();
    assert_eq!(
        sorted(prefs.ignore.unwrap()),
        strings(&["f-ban", "f-model-ban", "p-ban", "p-family-ban"])
    );
    assert_eq!(prefs.order, Some(strings(&["ok"])));
}

#[test]
fn each_priority_maps_to_its_openrouter_fields() {
    let with = |priority| ProjectRouting {
        routing: ProviderRouting::default(),
        priority: Some(priority),
    };
    let wire =
        |priority| serde_json::to_value(resolve(None, "a/b", Some(&with(priority)))).unwrap();
    assert_eq!(wire(RoutingPriority::Cost), json!({"sort": "price"}));
    assert_eq!(wire(RoutingPriority::Speed), json!({"sort": "throughput"}));
    assert_eq!(
        wire(RoutingPriority::Privacy),
        json!({"zdr": true, "data_collection": "deny"})
    );
}

// -- patterns

#[test]
fn patterns_match_models_globs_and_author_prefixes() {
    assert!(pattern_matches("z-ai/glm-*", "z-ai/glm-4.6"));
    assert!(!pattern_matches("z-ai/glm-*", "z-ai/gml-4.6"));
    assert!(pattern_matches("z-ai/", "z-ai/anything"));
    assert!(!pattern_matches("z-ai/", "z-aix/anything"));
    assert!(!pattern_matches("z-ai/glm-4.6", "z-ai/glm-4.6-air"));
    assert!(!pattern_matches("z-ai/glm-4.6*", "z-ai/glm-4x6"));
    assert!(pattern_matches("z-ai/*-air", "z-ai/glm-4.6-air"));
    assert!(!pattern_matches("z-ai/*-air*x", "z-ai/glm-air"));
}

#[test]
fn patterns_that_are_not_override_keys_are_refused() {
    for bad in ["", "glm", "openrouter:z-ai/glm", "*", "*/", "z ai/glm"] {
        assert!(!is_override_key(bad), "{bad:?}");
    }
    for good in ["z-ai/", "z-ai/glm-*", "z-ai/glm-4.6"] {
        assert!(is_override_key(good), "{good:?}");
    }
}

// -- normalize

#[test]
fn malformed_overrides_and_duplicate_patterns_are_dropped_not_thrown() {
    let n = normalize_routing(&json!({
        "global": {"banned": ["a", "a", 3], "favorites": "nope"},
        "overrides": [{"match": "z-ai/"}, {"match": "z-ai/", "banned": ["b"]}, {"match": "bad"}, null],
    }));
    assert_eq!(n.global, rule(&["a"], &[]));
    assert_eq!(n.overrides, vec![ov("z-ai/", &[], &[])]);
    assert!(normalize_routing(&json!("garbage")).overrides.is_empty());
}

#[test]
fn a_known_priority_is_kept_and_an_unknown_one_dropped() {
    assert_eq!(
        normalize_project_routing(&json!({"priority": "speed"})).priority,
        Some(RoutingPriority::Speed)
    );
    assert_eq!(
        normalize_project_routing(&json!({"priority": "fastest"})).priority,
        None
    );
}
