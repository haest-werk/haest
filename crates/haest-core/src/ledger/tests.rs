//! The turn caps reset whenever anything posts; the ledger is the ceiling that does not.

use std::{
    sync::{Mutex, OnceLock, mpsc},
    time::Duration,
};

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
    let ledger = ledger_with(2.5, 0).0;
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
    let mut refused = Vec::new();
    let outcome = agent.run("d", &mut refused).await;
    assert_eq!(p.requests().len(), 3);
    assert_eq!(end(&refused).0, StopReason::MaxTokens);
    let spent = Limit::Conversation(ConversationCap::Usd(2.5));
    assert_eq!(outcome.limit, Some(spent));
}

#[tokio::test]
async fn a_model_that_reports_no_price_is_capped_in_tokens() {
    let ledger = ledger_with(0.0, 30).0;
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
    assert_eq!(ledger.usd(), 0.0);
}

#[tokio::test]
async fn a_tools_own_spend_is_charged() {
    let paid = FnTool::arc("Echo", false, |_, _| async {
        Ok(ToolOutput {
            cost_usd: Some(1.0),
            ..ToolOutput::text("drew it")
        })
    });
    let ledger = ledger_with(0.0, 0).0;
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
    assert_eq!(ledger.usd(), 2.0);
}

#[test]
fn the_last_snapshot_restores_the_spend_and_a_raised_cap_and_raising_lets_it_continue() {
    let (a, a_saved) = ledger_with(5.0, 0);
    a.charge(&cost(6.0));
    assert!(a.exhausted().is_some());
    a.set_cap(ConversationCap::Usd(10.0));
    let (b, _) = ledger_with(5.0, 0);
    b.restore(a_saved.saved.lock().unwrap().last().unwrap());
    assert_eq!(b.usd(), 6.0);
    assert_eq!(b.caps().max_usd, 10.0);
    assert_eq!(b.exhausted(), None);
}

#[test]
fn the_human_is_warned_once_on_crossing_80_percent_and_again_after_a_raise() {
    let (ledger, rec) = ledger_with(10.0, 0);
    ledger.charge(&cost(8.0));
    ledger.charge(&cost(1.0));
    assert_eq!(rec.warnings.lock().unwrap().len(), 1);
    ledger.set_cap(ConversationCap::Usd(20.0));
    ledger.charge(&cost(8.0));
    assert_eq!(rec.warnings.lock().unwrap().len(), 2);
}

#[test]
fn the_summary_names_an_uncapped_measure_as_having_no_cap() {
    let (ledger, _) = ledger_with(10.0, 0);
    ledger.charge(&cost(1.0));
    let summary = ledger.summary();
    assert!(summary.contains("$1.00"), "{summary}");
    assert!(summary.contains("$10"), "{summary}");
    assert!(summary.contains("(no cap)"), "{summary}");
    assert!(!summary.contains("of no cap"), "{summary}");
}

#[test]
fn the_warning_names_only_the_cap_it_crossed() {
    let (ledger, rec) = ledger_with(10.0, 1000);
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
    // Set on the host's clone; the guard's, made earlier, says it too.
    let host = ledger.clone();
    host.set_remedies(Remedies::new(|limit| match limit {
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
    let (ledger, rec) = ledger_with(0.0, 0);
    ledger.charge(&Usage::default());
    assert!(rec.saved.lock().unwrap().is_empty());
    ledger.charge(&Usage {
        input: 3,
        ..Default::default()
    });
    assert_eq!(ledger.unpriced_tokens(), 3);
    assert_eq!(rec.saved.lock().unwrap().len(), 1);
}

#[test]
fn a_snapshot_names_only_the_caps_the_human_set_and_keeps_the_ts_shape() {
    let (ledger, _) = ledger_with(5.0, 100);
    assert_eq!(ledger.snapshot().caps, None);
    ledger.set_cap(ConversationCap::UnpricedTokens(200));
    let s = serde_json::to_value(ledger.snapshot()).unwrap();
    assert_eq!(
        s,
        json!({"usd": 0.0, "unpricedTokens": 0, "caps": {"maxUnpricedTokens": 200}})
    );
    assert_eq!(ledger.caps().max_usd, 5.0);
}

/// A snapshot at u64::MAX unpriced tokens takes another unpriced call
/// without overflowing.
#[test]
fn unpriced_tokens_saturate_at_u64_max() {
    let (l, _) = ledger_with(0.0, 0);
    l.restore(&LedgerSnapshot {
        usd: 0.0,
        unpriced_tokens: u64::MAX,
        caps: None,
    });
    l.charge(&Usage {
        input: 5,
        ..Default::default()
    });
    assert_eq!(l.unpriced_tokens(), u64::MAX);
}

/// Hooks that read the ledger they hear from: the warning shown with the
/// budget beside it, the snapshot checked against the ledger's own.
#[derive(Default)]
struct ReadsBack {
    ledger: OnceLock<Ledger>,
    heard: Mutex<Vec<String>>,
    saved: Mutex<Vec<(LedgerSnapshot, LedgerSnapshot)>>,
}

impl LedgerHooks for ReadsBack {
    fn persist(&self, s: &LedgerSnapshot) {
        let now = self.ledger.get().expect("set before any charge").snapshot();
        self.saved.lock().unwrap().push((s.clone(), now));
    }
    fn warn(&self, text: &str) {
        let summary = self.ledger.get().expect("set before any charge").summary();
        self.heard
            .lock()
            .unwrap()
            .push(format!("{text} | {summary}"));
    }
}

/// The hooks used to run with the ledger locked, so a warn hook that read
/// the ledger (its summary, beside the warning) hung the turn for good.
#[test]
fn a_hook_may_read_the_ledger_it_hears_from() {
    let hooks = Arc::new(ReadsBack::default());
    let caps = ConversationCaps {
        max_usd: 10.0,
        max_unpriced_tokens: 0,
    };
    let ledger = Ledger::new(caps, hooks.clone());
    let _ = hooks.ledger.set(ledger.clone());
    let (done, finished) = mpsc::channel();
    let charging = ledger.clone();
    std::thread::spawn(move || {
        charging.charge(&cost(8.5));
        charging.set_cap(ConversationCap::Usd(20.0));
        let _ = done.send(());
    });
    finished
        .recv_timeout(Duration::from_secs(5))
        .expect("the hooks ran with the ledger locked: deadlock");
    let heard = hooks.heard.lock().unwrap().clone();
    assert_eq!(heard.len(), 1, "{heard:?}");
    assert!(
        heard[0].ends_with(
            "| Conversation budget: $8.50 spent of $10; 0 unpriced tokens billed (no cap)."
        ),
        "{heard:?}"
    );
    let saved = hooks.saved.lock().unwrap().clone();
    assert_eq!(saved.len(), 2);
    assert!(saved.iter().all(|(given, read)| given == read));
}

/// The host's remedy ran with the ledger locked, so a remedy that names the
/// spend so far (it reads the ledger it is set on) hung the first charge that
/// warned, and the stop that named it.
#[test]
fn a_remedy_may_read_the_ledger_it_is_set_on() {
    let (ledger, rec) = ledger_with(1.0, 0);
    let reader = ledger.clone();
    ledger.set_remedies(Remedies::new(move |_| {
        Some(format!("(spent ${:.2})", reader.usd()))
    }));
    let (done, finished) = mpsc::channel();
    let charging = ledger.clone();
    std::thread::spawn(move || {
        charging.charge(&cost(0.85)); // past 80%: a warning, remedied
        charging.charge(&cost(0.25)); // past the cap: the stop, remedied
        let _ = done.send(charging.exhausted());
    });
    let stop = finished
        .recv_timeout(Duration::from_secs(5))
        .expect("the remedy ran with the ledger locked: deadlock");
    let warned = rec.warnings.lock().unwrap().clone();
    assert_eq!(warned.len(), 1, "{warned:?}");
    assert!(warned[0].ends_with(" (spent $0.85)"), "{warned:?}");
    let stop = stop.expect("past the cap");
    assert!(stop.ends_with(" (spent $1.10)"), "{stop}");
}

/// Hooks whose `persist` panics on its first call only.
#[derive(Default)]
struct PanicsOnce {
    panicked: std::sync::atomic::AtomicBool,
    saved: Mutex<Vec<LedgerSnapshot>>,
}

impl LedgerHooks for PanicsOnce {
    fn persist(&self, s: &LedgerSnapshot) {
        if !self
            .panicked
            .swap(true, std::sync::atomic::Ordering::SeqCst)
        {
            panic!("the host's disk is full, once");
        }
        self.saved.lock().unwrap().push(s.clone());
    }
}

/// `persist` ran with a lock of the ledger's held, so a hook that panicked
/// once poisoned it: every later charge added its spend and then panicked
/// with "ledger lock poisoned", and the conversation could not charge again.
#[test]
fn a_hook_that_panics_once_leaves_the_ledger_working() {
    let hooks = Arc::new(PanicsOnce::default());
    let caps = ConversationCaps {
        max_usd: 0.0,
        max_unpriced_tokens: 0,
    };
    let ledger = Ledger::new(caps, hooks.clone());
    let first = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        ledger.charge(&cost(0.1));
    }));
    assert!(first.is_err(), "the host's panic is the host's");
    let second = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        ledger.charge(&cost(0.2));
    }));
    assert!(
        second.is_ok(),
        "the next charge panicked: the ledger is poisoned"
    );
    assert_eq!(*hooks.saved.lock().unwrap(), [ledger.snapshot()]);
}

/// Clones are one ledger: what one charges, every other reads and stops on.
#[test]
fn every_clone_is_the_same_ledger() {
    let (ledger, rec) = ledger_with(1.0, 0);
    let spender = ledger.clone();
    spender.charge(&cost(1.0));
    assert_eq!(ledger.usd(), 1.0);
    assert!(ledger.exhausted().is_some());
    ledger.set_cap(ConversationCap::Usd(2.0));
    assert!(spender.exhausted().is_none());
    assert_eq!(rec.saved.lock().unwrap().len(), 2);
}

/// Charges from many threads at once: every one counts, and the snapshot
/// saved last is the ledger's final state.
#[test]
fn the_last_snapshot_saved_is_the_final_state_whatever_the_threads_did() {
    let (ledger, rec) = ledger_with(0.0, 0);
    let threads: Vec<_> = (0..8)
        .map(|_| {
            let l = ledger.clone();
            std::thread::spawn(move || {
                for _ in 0..100 {
                    l.charge(&cost(0.25));
                }
            })
        })
        .collect();
    for t in threads {
        t.join().expect("a charging thread panicked");
    }
    assert_eq!(ledger.usd(), 200.0);
    let saved = rec.saved.lock().unwrap();
    assert_eq!(saved.last(), Some(&ledger.snapshot()));
    let usds: Vec<f64> = saved.iter().map(|s| s.usd).collect();
    assert!(
        usds.windows(2).all(|w| w[0] < w[1]),
        "never an older one after a newer one"
    );
}
