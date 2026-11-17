//! Direct OpenAI-compatible providers (Paper Instruments' Ultramarine and any
//! server shaped like it): effort dialect, reasoning_content replay, booked cost.
//! Asserted on the wire body and the folded usage; no network.

use std::collections::HashMap;

use haest_core::{
    catalog::{ModelPricing, OpenRouterEffort, Reasoning},
    messages::{AssistantMessage, Message},
    provider::Effort,
};
use serde_json::{Value, json};

use super::{
    tests::{body_with, finished as chunk, run_body, usage_chunk},
    *,
};
use crate::testing::{chat_sse, request, request_with};

const MODEL: &str = "ultramarine-1-flash";

fn mandatory_low_high_max() -> Reasoning {
    use OpenRouterEffort as O;
    Reasoning {
        mandatory: true,
        efforts: vec![O::Low, O::High, O::Max],
    }
}

fn direct() -> CompatOptions {
    let mut opts = CompatOptions::new("direct", "k");
    opts.reasoning_effort = Some(mandatory_low_high_max());
    opts
}

pub(super) fn body_of(opts: &CompatOptions, effort: Option<Effort>) -> Value {
    body_with(opts, MODEL, effort)
}

pub(super) async fn final_message(opts: CompatOptions, chunks: &[Value]) -> AssistantMessage {
    run_body(opts, request(MODEL), chat_sse(chunks)).await.1
}

// -- effort dialect: top-level reasoning_effort, fitted to the accepted levels

#[test]
fn each_haest_level_lands_on_the_nearest_accepted_one_and_off_becomes_the_cheapest_when_mandatory()
{
    let sent: Vec<Value> = Effort::ALL
        .iter()
        .map(|e| body_of(&direct(), Some(*e))["reasoning_effort"].clone())
        .collect();
    assert_eq!(
        sent,
        [
            json!("low"),
            json!("low"),
            json!("high"),
            json!("high"),
            json!("max"),
            json!("max")
        ]
    );
}

#[test]
fn never_the_openrouter_reasoning_object_and_nothing_at_all_when_effort_is_unset() {
    assert!(
        body_of(&direct(), Some(Effort::Medium))
            .get("reasoning")
            .is_none()
    );
    assert!(body_of(&direct(), None).get("reasoning_effort").is_none());
}

#[test]
fn a_provider_that_lists_none_gets_it_for_off_and_one_with_no_dialect_gets_no_field() {
    use OpenRouterEffort as O;
    let mut optional = direct();
    optional.reasoning_effort = Some(Reasoning {
        mandatory: false,
        efforts: vec![O::None, O::Low, O::High],
    });
    assert_eq!(
        body_of(&optional, Some(Effort::Off))["reasoning_effort"],
        json!("none")
    );
    let mut plain = direct();
    plain.reasoning_effort = None;
    assert!(
        body_of(&plain, Some(Effort::High))
            .get("reasoning_effort")
            .is_none()
    );
}

// -- reasoning_content round trip

async fn streamed() -> AssistantMessage {
    final_message(
        direct(),
        &[
            chunk(json!({"reasoning_content": "look "}), None),
            chunk(json!({"reasoning_content": "first"}), None),
            chunk(json!({"tool_calls": [{"index": 0, "id": "call_1", "function": {"name": "Read", "arguments": "{}"}}]}), Some("tool_calls")),
        ],
    )
    .await
}

fn replayed_assistant(opts: &CompatOptions, msg: AssistantMessage) -> Value {
    let body = build_compat_body(
        opts,
        &request_with(MODEL, vec![Message::user("hi"), Message::Assistant(msg)]),
    );
    body["messages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["role"] == json!("assistant"))
        .unwrap()
        .clone()
}

#[tokio::test]
async fn the_streamed_reasoning_content_goes_back_to_the_same_provider() {
    let replayed = replayed_assistant(&direct(), streamed().await);
    assert_eq!(replayed["reasoning_content"], json!("look first"));
}

#[tokio::test]
async fn another_provider_never_receives_it() {
    let mut other = direct();
    other.id = "other".into();
    assert!(
        replayed_assistant(&other, streamed().await)
            .get("reasoning_content")
            .is_none()
    );
}

// -- cost booked from a configured price when the API returns none

fn usage() -> Value {
    json!({"prompt_tokens": 1_000_000, "completion_tokens": 2_000_000, "prompt_tokens_details": {"cached_tokens": 500_000}})
}

fn priced() -> CompatOptions {
    let mut opts = direct();
    opts.pricing = HashMap::from([(
        MODEL.to_string(),
        ModelPricing {
            input: 0.15,
            cached_input: 0.03,
            output: 0.5,
            cache_write: None,
        },
    )]);
    opts
}

pub(super) fn close(a: Option<f64>, b: f64) -> bool {
    a.is_some_and(|a| (a - b).abs() < 1e-10)
}

#[tokio::test]
async fn input_cached_input_and_output_each_at_their_own_rate() {
    let msg = final_message(
        priced(),
        &[
            chunk(json!({"content": "x"}), Some("stop")),
            usage_chunk(usage()),
        ],
    )
    .await;
    // 500K fresh * 0.15 + 500K cached * 0.03 + 2M out * 0.5 per 1M
    assert!(
        close(msg.usage.cost_usd, 0.075 + 0.015 + 1.0),
        "{:?}",
        msg.usage.cost_usd
    );
}

#[tokio::test]
async fn an_unpriced_model_books_nothing_and_a_reported_cost_is_never_overwritten() {
    let text = chunk(json!({"content": "x"}), Some("stop"));
    let unpriced = final_message(direct(), &[text.clone(), usage_chunk(usage())]).await;
    assert_eq!(unpriced.usage.cost_usd, None);
    let mut reported_usage = usage();
    reported_usage["cost"] = json!(7);
    let reported = final_message(priced(), &[text, usage_chunk(reported_usage)]).await;
    assert_eq!(reported.usage.cost_usd, Some(7.0));
}

/// A cost below zero is no cost a call can have: booked, it would take spend
/// off the ledger and lift a USD cap. It is not taken; the price is, as for
/// a server that reports none.
#[tokio::test]
async fn a_reported_cost_below_zero_is_not_taken() {
    let text = chunk(json!({"content": "x"}), Some("stop"));
    let mut reported_usage = usage();
    reported_usage["cost"] = json!(-1000);
    let priced_msg = final_message(
        priced(),
        &[text.clone(), usage_chunk(reported_usage.clone())],
    )
    .await;
    assert!(
        close(priced_msg.usage.cost_usd, 0.075 + 0.015 + 1.0),
        "{:?}",
        priced_msg.usage.cost_usd
    );
    let unpriced = final_message(direct(), &[text, usage_chunk(reported_usage)]).await;
    assert_eq!(unpriced.usage.cost_usd, None);
}
