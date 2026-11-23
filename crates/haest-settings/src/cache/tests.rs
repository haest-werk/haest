//! The `[cache]` table as a file writes it, per model, and as the policy a
//! conversation runs.

use std::time::Duration;

use haest_core::{
    Message,
    cache::{CachePlan, CachePolicy, CacheTtl, IdleAction, IdleState, RequestShape},
    catalog::ModelPricing,
};

use super::*;

fn read(toml_text: &str) -> Result<CacheSettings, toml::de::Error> {
    toml::from_str(toml_text)
}

fn plan(cache: &CacheSettings, spec: &str) -> CachePlan {
    cache.plan(&RequestShape::new(spec, &[], "sys", &[]))
}

#[test]
fn the_default_plans_what_haest_always_sent() {
    let cache = CacheSettings::default();
    assert_eq!(
        plan(&cache, "anthropic:claude-opus-5-5"),
        CachePlan::default()
    );
    assert_eq!(read("").unwrap(), cache);
}

#[test]
fn each_block_takes_off_five_minutes_or_an_hour() {
    let cache = read("system = \"1h\"\ntools = \"off\"\nhistory = \"5m\"").unwrap();
    let want =
        (CachePlan::none().with_system(CacheTtl::OneHour)).with_history(CacheTtl::FiveMinutes);
    assert_eq!(plan(&cache, "anthropic:claude-opus-5-5"), want);
}

#[test]
fn the_most_specific_model_override_wins_key_by_key() {
    let cache = read(
        r#"
keep_warm_for = 30
[models."openrouter:*"]
system = "off"
keep_warm = true
[models."openrouter:anthropic/*"]
system = "1h"
[models."openrouter:anthropic/claude-haiku-5.5"]
history = "off"
"#,
    )
    .unwrap();
    let haiku = cache.for_model("openrouter:anthropic/claude-haiku-5.5");
    assert_eq!(
        (haiku.system, haiku.history, haiku.keep_warm),
        (BreakpointTtl::OneHour, BreakpointTtl::Off, true)
    );
    assert_eq!(
        haiku.keep_warm_for,
        Duration::from_secs(1_800),
        "from [cache]"
    );
    let gemini = cache.for_model("openrouter:google/gemini-4-argon");
    assert_eq!(
        (gemini.system, gemini.history),
        (BreakpointTtl::Off, BreakpointTtl::FiveMinutes)
    );
    let direct = cache.for_model("anthropic:claude-haiku-5-5");
    assert_eq!(
        direct,
        CacheSettings {
            models: BTreeMap::new(),
            ..cache.clone()
        }
    );
}

#[test]
fn it_writes_only_what_differs_and_reads_back_the_same() {
    assert_eq!(toml::to_string(&CacheSettings::default()).unwrap(), "");
    let text = r#"
system = "1h"
keep_warm = true
keep_warm_for = 90.5
compact_before_expire = { min_context = "60%", instructions = "Keep the TODOs." }
[models."anthropic:*"]
tools = "off"
compact_before_expire = "off"
"#;
    let cache = read(text).unwrap();
    assert_eq!(cache.keep_warm_for, Duration::from_secs(5_430));
    let written = toml::to_string(&cache).unwrap();
    assert_eq!(read(&written).unwrap(), cache, "{written}");
    let tokens = read("compact_before_expire = { min_context = 120000 }").unwrap();
    let written = toml::to_string(&tokens).unwrap();
    assert_eq!(read(&written).unwrap(), tokens, "{written}");
}

#[test]
fn a_misspelt_key_or_a_value_it_does_not_take_is_refused() {
    for wrong in [
        "sytem = \"1h\"",
        "system = \"2h\"",
        "keep_warm_for = -5",
        "compact_before_expire = \"on\"",
        "compact_before_expire = true",
        "compact_before_expire = { instructions = \"no size\" }",
        "compact_before_expire = { min_context = \"160%\" }",
        "compact_before_expire = { min_context = -1 }",
        "compact_before_expire = { min_context = 100, when = \"idle\" }",
        "[models.\"anthropic:*\"]\nmodels = {}",
        // A key that is no pattern would match no model.
        "[models.\"a*b\"]\nsystem = \"off\"",
        "[models.\"openrouter:(google|openai)/*\"]\nsystem = \"off\"",
        "[models.\"\"]\nsystem = \"off\"",
    ] {
        assert!(read(wrong).is_err(), "{wrong}");
    }
    let why = read("[models.\"a*b\"]\nsystem = \"off\"")
        .unwrap_err()
        .to_string();
    assert!(
        why.contains("models.\"a*b\"") && why.contains("prefix ending in `*`"),
        "{why}"
    );
    assert_eq!(
        read("compact_before_expire = false")
            .unwrap()
            .compact_before_expire,
        CompactBeforeExpire::Off
    );
}

/// $3 in, $0.30 a read, $3.75 a 5m write: a read is 1/12.5 of a write, so
/// pings pay through the first 11.
fn priced() -> ModelPricing {
    ModelPricing::new(3.0, 0.3, 15.0).with_cache_write(3.75)
}

fn minutes(m: u64) -> Duration {
    Duration::from_secs(m * 60)
}

fn idle<'a>(price: &'a ModelPricing, away: Duration, tokens: u64) -> IdleState<'a> {
    IdleState::new(
        "anthropic:claude-sonnet-5-5",
        away,
        CacheTtl::FiveMinutes,
        tokens,
    )
    .with_context_window(200_000)
    .with_pricing(price)
}

/// A long first message, then the 13 short ones a compaction keeps.
fn long_history() -> Vec<Message> {
    let mut messages = vec![Message::user("x".repeat(400_000))];
    messages.extend((0..13).map(|_| Message::user("ok")));
    messages
}

#[test]
fn keep_warm_pings_while_it_pays_and_not_past_keep_warm_for() {
    let price = priced();
    let off = CacheSettings::default();
    assert_eq!(
        off.on_idle(&idle(&price, minutes(4), 50_000)),
        IdleAction::Nothing
    );
    let warm = read("keep_warm = true\nkeep_warm_for = 30").unwrap();
    assert_eq!(
        warm.on_idle(&idle(&price, minutes(4), 50_000)),
        IdleAction::Ping
    );
    assert_eq!(
        warm.on_idle(&idle(&price, minutes(31), 50_000)),
        IdleAction::Nothing
    );
    let unpriced = IdleState::new("m", minutes(4), CacheTtl::FiveMinutes, 50_000);
    assert_eq!(
        warm.on_idle(&unpriced),
        IdleAction::Nothing,
        "no price, no ping"
    );
    let long = read("keep_warm = true\nkeep_warm_for = 600").unwrap();
    assert_eq!(
        long.on_idle(&idle(&price, minutes(4 * 13), 50_000)),
        IdleAction::Nothing
    );
}

#[test]
fn compaction_comes_first_once_the_conversation_holds_min_context() {
    let (price, history) = (priced(), long_history());
    let cache = read(
        "keep_warm = true\ncompact_before_expire = { min_context = \"50%\", instructions = \"Keep the TODOs.\" }",
    )
    .unwrap();
    let past_half = idle(&price, minutes(4), 120_000).with_messages(&history);
    let IdleAction::Compact(options) = cache.on_idle(&past_half) else {
        panic!("a compaction past half the window");
    };
    assert_eq!(options.instructions.as_deref(), Some("Keep the TODOs."));
    let below = idle(&price, minutes(4), 90_000).with_messages(&history);
    assert_eq!(cache.on_idle(&below), IdleAction::Ping, "below it");
    let nothing_to_cut = idle(&price, minutes(4), 120_000).with_messages(&history[..1]);
    assert_eq!(
        cache.on_idle(&nothing_to_cut),
        IdleAction::Ping,
        "a compaction that does not pay"
    );
    let no_window = IdleState::new("m", minutes(4), CacheTtl::FiveMinutes, 900_000);
    assert_eq!(
        cache.on_idle(&no_window),
        IdleAction::Nothing,
        "no window, no share, no price"
    );
    let tokens = read("compact_before_expire = { min_context = 100000 }").unwrap();
    let no_window = IdleState::new("m", minutes(4), CacheTtl::FiveMinutes, 100_000)
        .with_pricing(&price)
        .with_messages(&history);
    assert!(matches!(tokens.on_idle(&no_window), IdleAction::Compact(_)));
}
