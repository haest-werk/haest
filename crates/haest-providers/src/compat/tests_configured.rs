//! Direct providers as configured: the `paperinstruments` built-in with its
//! catalog row, and a providers.json entry declaring the same effort dialect
//! and prices. Asserted on the wire body and the booked cost; no network.

use std::{collections::HashMap, sync::Arc};

use haest_core::{
    catalog::{InputModality, ModelPricing},
    provider::{Effort, ModelError, ModelResolver},
};
use serde_json::{Value, json};

use super::tests::{finished as chunk, usage_chunk};
use super::tests_direct::{body_of, close, final_message};
use crate::{
    catalog::Catalog,
    registry::{
        CustomProviderConfig, EntryProblem, ProviderRegistry, RegistryOptions,
        paper_instruments_options,
    },
    testing::FakeHttp,
};

// -- paperinstruments built-in + catalog row

fn pi_env() -> HashMap<String, String> {
    HashMap::from([("PAPERINSTRUMENTS_API_KEY".to_string(), "k".to_string())])
}

fn registry(keys: HashMap<String, String>) -> ProviderRegistry {
    let opts = RegistryOptions {
        http: FakeHttp::new([]),
        ..RegistryOptions::default()
    };
    ProviderRegistry::new(Arc::new(keys), Default::default(), opts)
}

#[test]
fn paperinstruments_resolves_with_its_key_and_names_the_env_var_without_one() {
    let spec = "paperinstruments:ultramarine-1-flash";
    assert_eq!(
        registry(pi_env()).resolve(spec).unwrap().provider.id(),
        "paperinstruments"
    );
    assert!(matches!(
        registry(HashMap::new()).resolve(spec),
        Err(ModelError::MissingKey { key, .. }) if key == "PAPERINSTRUMENTS_API_KEY"
    ));
}

#[tokio::test]
async fn paperinstruments_sends_a_level_the_api_accepts_and_prices_ultramarine_from_the_catalog() {
    let opts = paper_instruments_options(&pi_env()).unwrap();
    assert_eq!(
        opts.base_url.as_deref(),
        Some("https://api.paperinstruments.com/v1")
    );
    assert_eq!(
        body_of(&opts, Some(Effort::Medium))["reasoning_effort"],
        json!("high")
    );
    let msg = final_message(
        opts,
        &[
            chunk(json!({"content": "x"}), Some("stop")),
            usage_chunk(json!({"prompt_tokens": 1_000_000, "completion_tokens": 0})),
        ],
    )
    .await;
    assert!(close(msg.usage.cost_usd, 0.15), "{:?}", msg.usage.cost_usd);
}

#[test]
fn the_catalog_row_takes_images_and_says_reasoning_cannot_be_turned_off() {
    let c = Catalog::builtin();
    let spec = "paperinstruments:ultramarine-1-flash";
    assert!(
        c.input_modalities(spec)
            .unwrap()
            .contains(&InputModality::Image)
    );
    assert!(c.reasoning(spec).unwrap().mandatory);
}

// -- providers.json entries declare the same dialect + pricing

fn cfg() -> Value {
    json!({
        "type": "openai", "baseURL": "http://x/v1",
        "reasoningEffort": {"mandatory": true, "efforts": ["low", "high", "max"]},
        "pricing": {"m1": {"input": 1, "cachedInput": 0.5, "output": 2}},
    })
}

#[test]
fn effort_is_fitted_on_the_wire_and_cost_booked_from_the_entry() {
    let parsed = CustomProviderConfig::from_json(&cfg()).unwrap();
    let opts = parsed.compat_options("mine", &HashMap::new());
    assert_eq!(
        body_of(&opts, Some(Effort::Xhigh))["reasoning_effort"],
        json!("max")
    );
    assert_eq!(
        opts.pricing.get("m1"),
        Some(&ModelPricing {
            input: 1.0,
            cached_input: 0.5,
            output: 2.0,
            cache_write: None
        })
    );
}

#[test]
fn a_malformed_price_is_refused_at_load_not_booked_as_nan() {
    let rows = [
        json!({"input": 1}),
        json!({"input": 1, "cachedInput": 0.5, "output": 2, "cacheWrite": "free"}),
    ];
    for row in rows {
        let mut bad = cfg();
        bad["pricing"] = json!({ "m1": row });
        let err = CustomProviderConfig::from_json(&bad).unwrap_err();
        assert!(
            matches!(&err, EntryProblem::Pricing { model, .. } if model == "m1"),
            "{err}"
        );
    }
}
