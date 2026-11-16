//! The turn caps reset whenever anything posts; the ledger is the ceiling that does not.

use std::sync::Mutex;

use serde_json::json;

use super::*;
use crate::{
    agent::test_support::*,
    messages::StopReason,
    testing::{ScriptedProvider, text_answer},
    tool::ToolOutput,
};

#[derive(Default)]
struct Recorder {
    saved: Mutex<Vec<LedgerSnapshot>>,
    warnings: Mutex<Vec<String>>,
}

impl LedgerHooks for Recorder {
    fn persist(&self, s: &LedgerSnapshot) {
        self.saved.lock().unwrap().push(s.clone());
    }
    fn warn(&self, text: &str) {
        self.warnings.lock().unwrap().push(text.into());
    }
}

fn ledger_with(max_usd: f64, max_unpriced_tokens: u64) -> (Ledger, Arc<Recorder>) {
    let rec = Arc::new(Recorder::default());
    let caps = ConversationCaps {
        max_usd,
        max_unpriced_tokens,
    };
    (Ledger::new(caps, rec.clone()), rec)
}

fn shared(l: Ledger) -> Arc<std::sync::Mutex<Ledger>> {
    Arc::new(std::sync::Mutex::new(l))
}

fn cost(usd: f64) -> Usage {
    Usage {
        cost_usd: Some(usd),
        ..Default::default()
    }
}

fn answers(n: usize, usage: &Usage) -> Vec<crate::messages::AssistantMessage> {
    (0..n)
        .map(|_| with_usage(text_answer("done"), usage))
        .collect()
}

#[tokio::test]
async fn spend_across_turns_stops_the_conversation_and_posting_again_does_not_reset_it() {
    let ledger = shared(ledger_with(2.5, 0).0);
    let billed = Usage {
        input: 1,
        output: 1,
        cost_usd: Some(1.0),
        ..Default::default()
    };
    let p = ScriptedProvider::new(answers(10, &billed));
    let (mut agent, _) = agent_with(
        &p,
        Opts {
            ledger: Some(ledger),
            ..Default::default()
        },
    );
    for msg in ["a", "b", "c"] {
        collect(&mut agent, msg).await;
    }
    let refused = collect(&mut agent, "d").await;
    assert_eq!(p.requests().len(), 3);
    assert_eq!(end(&refused).0, StopReason::MaxTokens);
}

#[tokio::test]
async fn a_model_that_reports_no_price_is_capped_in_tokens() {
    let ledger = shared(ledger_with(0.0, 30).0);
    let unpriced = Usage {
        input: 10,
        output: 5,
        ..Default::default()
    };
    let p = ScriptedProvider::new(answers(10, &unpriced));
    let (mut agent, _) = agent_with(
        &p,
        Opts {
            ledger: Some(ledger.clone()),
            ..Default::default()
        },
    );
    for msg in ["a", "b", "c"] {
        collect(&mut agent, msg).await;
    }
    assert_eq!(p.requests().len(), 2);
    assert_eq!(ledger.lock().unwrap().usd, 0.0);
}

#[tokio::test]
async fn a_tools_own_spend_is_charged() {
    let paid = FnTool::arc("Echo", false, |_, _| async {
        Ok(ToolOutput {
            cost_usd: Some(1.0),
            ..ToolOutput::text("drew it")
        })
    });
    let ledger = shared(ledger_with(0.0, 0).0);
    let billed = Usage {
        input: 1,
        output: 1,
        cost_usd: Some(0.5),
        ..Default::default()
    };
    let p = ScriptedProvider::new([
        with_usage(echo("t", json!({})), &billed),
        with_usage(text_answer("done"), &billed),
    ]);
    let (mut agent, _) = agent_with(
        &p,
        Opts {
            tools: Some(vec![paid]),
            ledger: Some(ledger.clone()),
            ..Default::default()
        },
    );
    collect(&mut agent, "go").await;
    assert_eq!(ledger.lock().unwrap().usd, 2.0);
}

#[test]
fn the_last_snapshot_restores_the_spend_and_a_raised_cap_and_raising_lets_it_continue() {
    let (mut a, a_saved) = ledger_with(5.0, 0);
    a.charge(&cost(6.0));
    assert!(a.exhausted().is_some());
    a.set_cap(ConversationCap::Usd(10.0));
    let (mut b, _) = ledger_with(5.0, 0);
    b.restore(a_saved.saved.lock().unwrap().last().unwrap());
    assert_eq!(b.usd, 6.0);
    assert_eq!(b.caps().max_usd, 10.0);
    assert_eq!(b.exhausted(), None);
}

#[test]
fn the_human_is_warned_once_on_crossing_80_percent_and_again_after_a_raise() {
    let (mut ledger, rec) = ledger_with(10.0, 0);
    ledger.charge(&cost(8.0));
    ledger.charge(&cost(1.0));
    assert_eq!(rec.warnings.lock().unwrap().len(), 1);
    ledger.set_cap(ConversationCap::Usd(20.0));
    ledger.charge(&cost(8.0));
    assert_eq!(rec.warnings.lock().unwrap().len(), 2);
}

#[test]
fn the_summary_names_an_uncapped_measure_as_having_no_cap() {
    let (mut ledger, _) = ledger_with(10.0, 0);
    ledger.charge(&cost(1.0));
    let summary = ledger.summary();
    assert!(summary.contains("$1.00"), "{summary}");
    assert!(summary.contains("$10"), "{summary}");
    assert!(summary.contains("(no cap)"), "{summary}");
    assert!(!summary.contains("of no cap"), "{summary}");
}

#[test]
fn the_warning_names_only_the_cap_it_crossed() {
    let (mut ledger, rec) = ledger_with(10.0, 1000);
    ledger.charge(&cost(8.0));
    ledger.charge(&Usage {
        input: 900,
        ..Default::default()
    });
    let warned = rec.warnings.lock().unwrap().clone();
    assert_eq!(warned.len(), 2, "{warned:?}");
    assert!(warned[0].contains("$8.00"), "{warned:?}");
    assert!(!warned[0].contains("tokens"), "{warned:?}");
    assert!(warned[1].contains("900"), "{warned:?}");
    assert!(!warned[1].contains('$'), "{warned:?}");
}

#[test]
fn the_warning_and_the_stop_end_with_the_hosts_remedy_for_the_cap_in_force() {
    let (ledger, rec) = ledger_with(10.0, 0);
    let mut ledger = ledger.with_remedies(Remedies::new(|limit| match limit {
        Limit::Conversation(ConversationCap::Usd(cap)) => Some(format!("RAISE-FROM-{cap}")),
        _ => None,
    }));
    ledger.set_cap(ConversationCap::Usd(5.0));
    ledger.charge(&cost(4.5));
    let warned = rec.warnings.lock().unwrap().clone();
    assert!(warned[0].ends_with(" RAISE-FROM-5"), "{warned:?}");
    ledger.charge(&cost(1.0));
    let stop = ledger.exhausted().unwrap();
    assert!(stop.ends_with(" RAISE-FROM-5"), "{stop}");
}

#[test]
fn a_call_that_billed_nothing_is_not_persisted() {
    let (mut ledger, rec) = ledger_with(0.0, 0);
    ledger.charge(&Usage::default());
    assert!(rec.saved.lock().unwrap().is_empty());
    ledger.charge(&Usage {
        input: 3,
        ..Default::default()
    });
    assert_eq!(ledger.unpriced_tokens, 3);
    assert_eq!(rec.saved.lock().unwrap().len(), 1);
}

#[test]
fn a_snapshot_names_only_the_caps_the_human_set_and_keeps_the_ts_shape() {
    let (mut ledger, _) = ledger_with(5.0, 100);
    assert_eq!(ledger.snapshot().caps, None);
    ledger.set_cap(ConversationCap::UnpricedTokens(200));
    let s = serde_json::to_value(ledger.snapshot()).unwrap();
    assert_eq!(
        s,
        json!({"usd": 0.0, "unpricedTokens": 0, "caps": {"maxUnpricedTokens": 200}})
    );
    assert_eq!(ledger.caps().max_usd, 5.0);
}
