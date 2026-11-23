//! The break-even of a keep-warm ping, and what a pre-warm sends, books and
//! leaves alone.

use std::time::Duration;

use serde_json::json;
use tokio_util::sync::CancellationToken;

use super::{warm::warm_messages, *};
use crate::{
    Agent, AgentDeps, AgentOptions, SharedDeps, ShutWorkspace, Tools,
    agent::test_support::{Opts, agent_with, collect, echo},
    caps::ConversationCaps,
    ledger::{Ledger, NoLedgerHooks},
    messages::{AssistantMessage, Message, StopReason},
    provider::{Provider, ProviderRequest, ProviderStream, ResolvedModel},
    testing::{ScriptedProvider, text_answer},
};

/// $2 in, $0.10 a read, $2.50 a 5m write, $4 a 1h write: a read is 1/25 of
/// a 5m write, so the 24th ping in a row is the first that does not pay.
fn priced() -> ModelPricing {
    ModelPricing::new(2.0, 0.1, 10.0)
        .with_cache_write(2.5)
        .with_cache_write_1h(4.0)
}

fn pings(ttl: CacheTtl, n: u32) -> Duration {
    ttl.ping_every() * n
}

#[test]
fn no_price_for_the_ttl_means_no_ping() {
    let minute = Duration::from_secs(60);
    assert!(!ping_pays(None, CacheTtl::FiveMinutes, minute));
    let no_1h = ModelPricing::new(2.0, 0.1, 10.0).with_cache_write(2.5);
    assert!(!ping_pays(Some(&no_1h), CacheTtl::OneHour, minute));
    assert!(ping_pays(Some(&no_1h), CacheTtl::FiveMinutes, minute));
    // A model whose read costs as much as a rewrite saves nothing by reading.
    let flat = ModelPricing::new(1.0, 1.0, 1.0);
    assert!(!ping_pays(Some(&flat), CacheTtl::FiveMinutes, minute));
}

#[test]
fn pings_pay_through_a_short_idle_and_stop_after_a_long_one() {
    let p = priced();
    let five = CacheTtl::FiveMinutes;
    assert!(ping_pays(Some(&p), five, Duration::ZERO), "the first ping");
    assert!(ping_pays(Some(&p), five, pings(five, 23)));
    assert!(!ping_pays(Some(&p), five, pings(five, 24)));
    assert!(!ping_pays(Some(&p), five, Duration::from_secs(86_400)));
    // 1h: a read is 1/40 of its write, so the 39th ping is the first that
    // does not pay.
    let hour = CacheTtl::OneHour;
    assert!(ping_pays(Some(&p), hour, pings(hour, 38)));
    assert!(!ping_pays(Some(&p), hour, pings(hour, 39)));
}

#[test]
fn a_5m_write_without_its_own_price_is_weighed_as_input() {
    // A read is 1/20 of the $2 input a write costs here: 18 pings pay.
    let p = ModelPricing::new(2.0, 0.1, 10.0);
    let five = CacheTtl::FiveMinutes;
    assert!(ping_pays(Some(&p), five, pings(five, 18)));
    assert!(!ping_pays(Some(&p), five, pings(five, 20)));
}

#[test]
fn a_pre_warm_ends_where_the_last_request_ended() {
    let user = Message::user("hi");
    let answer = Message::Assistant(text_answer("hello"));
    assert_eq!(warm_messages(&[user.clone(), answer]), vec![user.clone()]);
    assert!(warm_messages(&[]).is_empty());
    // A call a dropped turn left is answered as the next turn answers it.
    let call = Message::Assistant(echo("c1", json!({})));
    let warm = warm_messages(&[user.clone(), call.clone()]);
    let [u, c, Message::Tool(result)] = warm.as_slice() else {
        panic!("user, call, interrupted result: {warm:?}");
    };
    assert_eq!((u, c), (&user, &call));
    assert!(result.is_error);
    assert_eq!(result.tool_call_id, "c1");
}

/// A turn of one tool call and an answer, on a ledger; then a pre-warm whose
/// reply read 4000 tokens from the cache at $0.01.
async fn one_turn_then_warm(
    ledger: &Ledger,
) -> (Agent, ScriptedProvider, Result<WarmOutcome, WarmError>) {
    let mut ping = AssistantMessage::start("scripted", "m1");
    ping.usage.cache_read = 4_000;
    ping.usage.cost_usd = Some(0.01);
    ping.stop_reason = StopReason::MaxTokens;
    let p = ScriptedProvider::new([echo("c1", json!({"x": 1})), text_answer("done"), ping]);
    let opts = Opts {
        ledger: Some(ledger.clone()),
        ..Default::default()
    };
    let (mut agent, _hooks) = agent_with(&p, opts);
    collect(&mut agent, "go").await;
    let warmed = agent.warm_cache(&CancellationToken::new()).await;
    (agent, p, warmed)
}

#[tokio::test]
async fn a_pre_warm_resends_the_last_request_and_leaves_history_alone() {
    let ledger = Ledger::new(ConversationCaps::default(), NoLedgerHooks);
    let (agent, p, warmed) = one_turn_then_warm(&ledger).await;
    let warmed = warmed.unwrap();
    assert_eq!(warmed.cache, WarmCache::Read);
    assert_eq!(agent.history().len(), 4, "go, the call, its result, done");
    let sent = p.requests();
    let (last, warm) = (&sent[1], &sent[2]);
    assert!(warm.prewarm && !last.prewarm);
    assert_eq!(warm.messages, last.messages);
    assert_eq!(
        (&warm.system, &warm.tools, warm.effort, &warm.cache),
        (&last.system, &last.tools, last.effort, &last.cache)
    );
    // The turn's calls had no price; the pre-warm's is on the ledger.
    assert_eq!(warmed.usage.cost_usd, Some(0.01));
    assert!((ledger.usd() - 0.01).abs() < 1e-12);
}

#[tokio::test]
async fn a_failed_pre_warm_is_still_booked() {
    let ledger = Ledger::new(ConversationCaps::default(), NoLedgerHooks);
    let mut failed = AssistantMessage::start("scripted", "m1");
    failed.stop_reason = StopReason::Error;
    failed.error_message = Some("overloaded".into());
    failed.usage.cost_usd = Some(0.02);
    let p = ScriptedProvider::new([text_answer("hi"), failed]);
    let opts = Opts {
        ledger: Some(ledger.clone()),
        ..Default::default()
    };
    let (mut agent, _hooks) = agent_with(&p, opts);
    collect(&mut agent, "hello").await;
    let err = agent
        .warm_cache(&CancellationToken::new())
        .await
        .unwrap_err();
    let WarmError::Failed { reason, usage } = err else {
        panic!("a failed pre-warm: {err:?}");
    };
    assert_eq!(
        (reason.as_str(), usage.cost_usd),
        ("overloaded", Some(0.02))
    );
    assert!((ledger.usd() - 0.02).abs() < 1e-12);
    assert_eq!(agent.history().len(), 2);
}

#[tokio::test]
async fn nothing_is_sent_without_a_request_to_warm() {
    let p = ScriptedProvider::new([]);
    let (mut agent, _hooks) = agent_with(&p, Opts::default());
    let err = agent
        .warm_cache(&CancellationToken::new())
        .await
        .unwrap_err();
    assert!(matches!(err, WarmError::NoRequest), "{err:?}");
    assert!(p.requests().is_empty());
}

/// A provider that takes no pre-warm (the default) and counts what it is sent.
#[derive(Clone, Default)]
pub(super) struct Cold(pub(super) std::sync::Arc<std::sync::atomic::AtomicUsize>);

impl Provider for Cold {
    fn id(&self) -> &str {
        "cold"
    }
    fn stream(&self, req: ProviderRequest) -> ProviderStream {
        self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let mut m = AssistantMessage::start("cold", &req.model);
        m.stop_reason = StopReason::EndTurn;
        Box::pin(futures::stream::iter([crate::ProviderEvent::Done(m)]))
    }
}

#[tokio::test]
async fn a_provider_without_pre_warm_refuses_before_anything_is_sent() {
    let cold = Cold::default();
    let model = ResolvedModel::new(cold.clone(), "m");
    let shared = SharedDeps::new(model, "/", ShutWorkspace);
    let opts = AgentOptions::new("cold:m").with_history(vec![Message::user("hi")]);
    let mut agent = Agent::new(AgentDeps::new(shared, Tools::new(), "sys"), opts);
    let err = agent
        .warm_cache(&CancellationToken::new())
        .await
        .unwrap_err();
    let WarmError::Refused { model_spec, source } = err else {
        panic!("a refusal: {err:?}");
    };
    assert_eq!(model_spec, "cold:m");
    assert!(matches!(source, PrewarmRefused::Unsupported { provider } if provider == "cold"));
    assert_eq!(cold.0.load(std::sync::atomic::Ordering::SeqCst), 0);
}

#[tokio::test]
async fn the_cache_policy_and_key_ride_on_every_model_call() {
    let p = ScriptedProvider::new([text_answer("hi"), text_answer("hi again")]);
    let shared = SharedDeps::new(p.resolver(), "/", ShutWorkspace);
    let opts = AgentOptions::new("scripted:m")
        .with_cache_policy(CachePlan::standard(CacheTtl::OneHour))
        .with_cache_key("session-1");
    let mut agent = Agent::new(AgentDeps::new(shared, Tools::new(), "sys"), opts);
    collect(&mut agent, "hello").await;
    agent.set_cache_policy(CachePlan::none().with_history(CacheTtl::FiveMinutes));
    agent.set_cache_key("session-2");
    collect(&mut agent, "again").await;
    let sent: Vec<_> = (p.requests().into_iter())
        .map(|r| (r.cache.system, r.cache.history, r.cache_key))
        .collect();
    let hour = Some(CacheTtl::OneHour);
    assert_eq!(
        sent,
        [
            (hour, hour, Some("session-1".to_string())),
            (
                None,
                Some(CacheTtl::FiveMinutes),
                Some("session-2".to_string())
            ),
        ]
    );
    assert_eq!(agent.last_cache_plan(), Some(&p.requests()[1].cache));
}
