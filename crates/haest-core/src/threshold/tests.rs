//! Thresholds: when a watch fires each kind of rearm, what a site refuses,
//! and a host's own threshold in a whole turn (remind, warn, stop, the
//! context's size).

use std::sync::Arc;

use super::*;
use crate::{
    agent::test_support::*,
    events::LoopEvent,
    messages::{Message, StopReason, Usage},
    testing::{ScriptedProvider, text_answer},
};

/// A threshold made of its four parts; it says `says` and the reading.
struct Host {
    meter: Meter,
    mark: Mark,
    action: Action,
    rearm: Rearm,
    says: &'static str,
}

impl Threshold for Host {
    fn meter(&self) -> Meter {
        self.meter
    }
    fn mark(&self) -> Mark {
        self.mark
    }
    fn action(&self) -> Action {
        self.action
    }
    fn rearm(&self) -> Rearm {
        self.rearm
    }
    fn text(&self, r: &Reading) -> String {
        format!("{} at {}", self.says, r.value)
    }
}

fn host(meter: Meter, mark: Mark, action: Action, rearm: Rearm) -> Arc<dyn Threshold> {
    named(meter, mark, action, rearm, "HOST")
}

fn named(
    meter: Meter,
    mark: Mark,
    action: Action,
    rearm: Rearm,
    says: &'static str,
) -> Arc<dyn Threshold> {
    Arc::new(Host {
        meter,
        mark,
        action,
        rearm,
        says,
    })
}

/// Reminds at half of 10 steps, rearming as `rearm` says.
fn half_the_steps(rearm: Rearm) -> Watch<TurnSite> {
    let t = host(Meter::Steps, Mark::share(0.5), Action::Remind, rearm);
    Watch::new(Thresholds::none().with(t).unwrap())
}

/// The steps meter at `steps` of 10.
fn steps(steps: f64) -> impl Fn(Meter) -> Option<Reading> {
    move |meter| {
        (meter == Meter::Steps).then_some(Reading {
            meter,
            value: steps,
            cap: Some(10.0),
            limit: None,
        })
    }
}

fn fires(watch: &mut Watch<TurnSite>, at: f64) -> usize {
    watch.check(steps(at), Action::Remind).len()
}

#[test]
fn never_fires_once_for_as_long_as_its_site_lives() {
    let mut w = half_the_steps(Rearm::Never);
    assert_eq!(fires(&mut w, 4.0), 0);
    assert_eq!(fires(&mut w, 5.0), 1);
    assert_eq!(fires(&mut w, 6.0), 0);
    w.next_turn();
    assert_eq!(fires(&mut w, 1.0), 0);
    assert_eq!(fires(&mut w, 6.0), 0);
}

#[test]
fn next_turn_fires_once_a_turn() {
    let mut w = half_the_steps(Rearm::NextTurn);
    assert_eq!(fires(&mut w, 5.0), 1);
    assert_eq!(fires(&mut w, 6.0), 0);
    // Falling under the mark does not re-arm it; the next turn does.
    assert_eq!(fires(&mut w, 1.0), 0);
    assert_eq!(fires(&mut w, 6.0), 0);
    w.next_turn();
    assert_eq!(fires(&mut w, 6.0), 1);
}

#[test]
fn below_mark_fires_again_only_after_the_meter_falls_back_under_it() {
    let mut w = half_the_steps(Rearm::BelowMark);
    assert_eq!(fires(&mut w, 5.0), 1);
    assert_eq!(fires(&mut w, 6.0), 0);
    w.next_turn();
    assert_eq!(fires(&mut w, 6.0), 0);
    assert_eq!(fires(&mut w, 4.0), 0);
    assert_eq!(fires(&mut w, 5.0), 1);
}

#[test]
fn every_check_fires_at_every_check_past_the_mark() {
    let mut w = half_the_steps(Rearm::EveryCheck);
    assert_eq!(fires(&mut w, 4.0), 0);
    assert_eq!(fires(&mut w, 5.0), 1);
    assert_eq!(fires(&mut w, 5.0), 1);
    assert_eq!(fires(&mut w, 9.0), 1);
}

#[test]
fn a_check_for_another_action_leaves_a_threshold_armed() {
    let mut w = half_the_steps(Rearm::Never);
    assert!(w.check(steps(6.0), Action::Warn).is_empty());
    assert_eq!(fires(&mut w, 6.0), 1);
}

#[test]
fn a_meter_the_site_cannot_read_is_skipped_and_stays_armed() {
    let mut w = half_the_steps(Rearm::Never);
    assert!(w.check(|_| None, Action::Remind).is_empty());
    assert_eq!(fires(&mut w, 6.0), 1);
}

#[test]
fn a_share_needs_a_cap_and_a_value_does_not() {
    let no_cap = |value: f64| Reading {
        meter: Meter::Steps,
        value,
        cap: None,
        limit: None,
    };
    assert!(!Mark::share(0.5).reached(&no_cap(1e12)));
    assert!(Mark::at(3.0).reached(&no_cap(3.0)));
    assert!(!Mark::at(3.0).reached(&no_cap(2.9)));
    // Whichever comes first.
    let both = Mark::share(0.5).or_at(3.0);
    let capped = |value| Reading {
        cap: Some(100.0),
        ..no_cap(value)
    };
    assert!(both.reached(&capped(3.0)));
    assert!(Mark::share(0.5).or_at(80.0).reached(&capped(50.0)));
}

#[test]
fn settle_takes_a_reading_as_heard_and_fires_nothing() {
    let all = [
        Rearm::Never,
        Rearm::NextTurn,
        Rearm::BelowMark,
        Rearm::EveryCheck,
    ]
    .map(|r| host(Meter::Steps, Mark::share(0.5), Action::Remind, r));
    let mut w = Watch::new(Thresholds::<TurnSite>::new(all).unwrap());
    w.settle(steps(6.0));
    // Past the mark counts as fired: only the level-triggered one fires.
    let due = w.check(steps(7.0), Action::Remind);
    assert_eq!(due.len(), 1);
    assert_eq!(due[0].threshold.rearm(), Rearm::EveryCheck);
    // Under the mark, a below-mark threshold re-arms.
    w.settle(steps(2.0));
    let due: Vec<Rearm> = w
        .check(steps(6.0), Action::Remind)
        .iter()
        .map(|c| c.threshold.rearm())
        .collect();
    assert_eq!(due, [Rearm::BelowMark, Rearm::EveryCheck]);
}

#[test]
fn follow_keeps_what_fired_and_arms_what_is_new() {
    let a = host(Meter::Steps, Mark::share(0.5), Action::Remind, Rearm::Never);
    let mut w = Watch::new(Thresholds::<TurnSite>::new([a.clone()]).unwrap());
    assert_eq!(fires(&mut w, 6.0), 1);
    // A new one just like `a`, and `a` itself: only the new one fires.
    let twin = host(Meter::Steps, Mark::share(0.5), Action::Remind, Rearm::Never);
    w.follow(&Thresholds::new([twin.clone(), a]).unwrap());
    let due = w.check(steps(6.0), Action::Remind);
    assert_eq!(due.len(), 1);
    assert!(Arc::ptr_eq(&due[0].threshold, &twin));
}

#[test]
fn a_site_refuses_what_it_cannot_check_and_says_why() {
    let turns = host(Meter::Turns, Mark::at(3.0), Action::Remind, Rearm::Never);
    let err = Thresholds::<TurnSite>::none().with(turns).unwrap_err();
    assert_eq!(
        err,
        NotServed {
            site: "during a turn",
            meter: Meter::Turns,
            action: Action::Remind
        }
    );
    assert_eq!(
        err.to_string(),
        "a threshold that would remind on turns is not checked during a turn"
    );
    let compact = host(
        Meter::ContextTokens,
        Mark::at(1.0),
        Action::Compact,
        Rearm::EveryCheck,
    );
    assert!(Thresholds::<TurnSite>::none().with(compact).is_err());
    // The ledger only warns, and only on the conversation's meters.
    let remind = host(
        Meter::Usd(Scope::Conversation),
        Mark::at(1.0),
        Action::Remind,
        Rearm::Never,
    );
    let turn_usd = host(
        Meter::Usd(Scope::Turn),
        Mark::at(1.0),
        Action::Warn,
        Rearm::Never,
    );
    let warn = host(
        Meter::Tokens(Scope::Conversation),
        Mark::at(1.0),
        Action::Warn,
        Rearm::Never,
    );
    assert!(Thresholds::<LedgerSite>::new([remind]).is_err());
    assert!(Thresholds::<LedgerSite>::new([turn_usd]).is_err());
    assert_eq!(Thresholds::<LedgerSite>::new([warn]).unwrap().len(), 1);
}

/// The defaults are what haest did before thresholds: the same marks,
/// actions and rearms on the same meters.
#[test]
fn the_defaults_are_the_guards_nudge_and_the_ledgers_warning() {
    let parts = |t: &Arc<dyn Threshold>| (t.meter(), t.mark(), t.action(), t.rearm());
    let turn: Vec<_> = Thresholds::<TurnSite>::default()
        .iter()
        .map(parts)
        .collect();
    let nudge = |m| (m, Mark::share(0.5), Action::Remind, Rearm::NextTurn);
    assert_eq!(
        turn,
        [
            nudge(Meter::Steps),
            nudge(Meter::WallTime),
            nudge(Meter::Usd(Scope::Turn)),
            nudge(Meter::Tokens(Scope::Turn)),
        ]
    );
    let ledger: Vec<_> = Thresholds::<LedgerSite>::default()
        .iter()
        .map(parts)
        .collect();
    let warn = |m| (m, Mark::share(0.8), Action::Warn, Rearm::BelowMark);
    assert_eq!(
        ledger,
        [
            warn(Meter::Usd(Scope::Conversation)),
            warn(Meter::Tokens(Scope::Conversation)),
        ]
    );
}

// --- In a whole turn -------------------------------------------------------

/// The guard's nudges and `more`, on an agent with Echo and `opts`.
fn agent(
    p: &Arc<ScriptedProvider>,
    opts: Opts,
    more: Vec<Arc<dyn Threshold>>,
) -> (crate::Agent, Arc<TestHooks>) {
    let (mut agent, hooks) = agent_with(p, opts);
    let mut thresholds = Thresholds::default();
    for t in more {
        thresholds = thresholds.with(t).unwrap();
    }
    agent.deps_mut().shared.thresholds = thresholds;
    (agent, hooks)
}

fn guard_texts(events: &[LoopEvent]) -> Vec<String> {
    events
        .iter()
        .filter_map(|e| match e {
            LoopEvent::Guard { text } => Some(text.clone()),
            _ => None,
        })
        .collect()
}

/// Every user message the model was sent, across the turn's requests.
fn sent_to_model(p: &ScriptedProvider) -> Vec<String> {
    let last = p.requests().last().cloned().unwrap();
    last.messages
        .iter()
        .filter_map(|m| match m {
            Message::User(u) => Some(u.content.clone()),
            _ => None,
        })
        .collect()
}

fn echoes(n: usize) -> Vec<crate::messages::AssistantMessage> {
    let mut script: Vec<_> = (0..n)
        .map(|i| echo(&format!("c{i}"), serde_json::json!({"v": i})))
        .collect();
    script.push(text_answer("done"));
    script
}

#[tokio::test]
async fn a_hosts_reminder_reaches_the_model_in_one_message_with_the_guards_nudge() {
    let p = ScriptedProvider::new(echoes(3));
    let mine = host(Meter::Steps, Mark::at(2.0), Action::Remind, Rearm::NextTurn);
    let (mut agent, _) = agent(&p, capped(|c| c.max_turn_steps = 4), vec![mine]);
    let events = collect(&mut agent, "go").await;
    let said = guard_texts(&events);
    assert_eq!(said.len(), 1, "{said:?}");
    let text = &said[0];
    assert!(
        text.starts_with("[haest guard] You have made 2 model calls"),
        "{text}"
    );
    assert!(text.contains("HOST at 2"), "{text}");
    // The guard's way out, once, at the end.
    assert_eq!(text.matches(WAY_OUT).count(), 1, "{text}");
    assert!(text.ends_with(WAY_OUT), "{text}");
    assert!(sent_to_model(&p).contains(text));
}

#[tokio::test]
async fn a_hosts_warning_reaches_the_human_through_the_hook_and_never_the_model() {
    let paid = Usage {
        cost_usd: Some(0.6),
        ..Default::default()
    };
    let p = ScriptedProvider::new([
        with_usage(echo("a", serde_json::json!({})), &paid),
        echo("b", serde_json::json!({})),
        text_answer("done"),
    ]);
    let mine = host(
        Meter::Usd(Scope::Turn),
        Mark::at(0.5),
        Action::Warn,
        Rearm::NextTurn,
    );
    let (mut agent, hooks) = agent(&p, Opts::default(), vec![mine]);
    let events = collect(&mut agent, "go").await;
    assert_eq!(*hooks.warnings.lock().unwrap(), ["HOST at 0.6"]);
    assert!(guard_texts(&events).is_empty());
    assert!(!sent_to_model(&p).iter().any(|m| m.contains("HOST")));
}

#[tokio::test]
async fn a_hosts_stop_ends_the_turn_before_the_next_model_call_with_the_hosts_remedy() {
    let p = ScriptedProvider::new(echoes(5));
    let mine = host(Meter::Steps, Mark::at(2.0), Action::Stop, Rearm::Never);
    let (mut agent, _) = agent(&p, capped(|c| c.max_turn_steps = 10), vec![mine]);
    agent.deps_mut().shared.remedies =
        Remedies::new(|limit| matches!(limit, Limit::TurnSteps(10)).then(|| "RAISE IT".into()));
    let events = collect(&mut agent, "go").await;
    assert_eq!(p.requests().len(), 2);
    let (stop, _, error) = end(&events);
    assert_eq!(stop, StopReason::MaxTokens);
    assert_eq!(error.as_deref(), Some("Turn stopped: HOST at 2 RAISE IT"));
}

#[tokio::test]
async fn a_context_threshold_reads_the_last_call_against_the_models_window() {
    let sent = |input| Usage {
        input,
        ..Default::default()
    };
    let p = ScriptedProvider::new([
        with_usage(echo("a", serde_json::json!({})), &sent(400)),
        with_usage(echo("b", serde_json::json!({})), &sent(600)),
        with_usage(echo("c", serde_json::json!({})), &sent(700)),
        text_answer("done"),
    ]);
    let mine = host(
        Meter::ContextTokens,
        Mark::share(0.5),
        Action::Remind,
        Rearm::BelowMark,
    );
    let (mut agent, _) = agent(&p, capped(|c| c.context_window = 1000), vec![mine]);
    let events = collect(&mut agent, "go").await;
    // Past half the window at 600, and not again at 700: it never fell under.
    assert_eq!(guard_texts(&events), ["[haest guard] HOST at 600"]);
}

#[tokio::test]
async fn a_share_of_an_unknown_window_never_fires() {
    let big = Usage {
        input: 1_000_000,
        ..Default::default()
    };
    let p = ScriptedProvider::new([
        with_usage(echo("a", serde_json::json!({})), &big),
        text_answer("done"),
    ]);
    let mine = host(
        Meter::ContextTokens,
        Mark::share(0.1),
        Action::Remind,
        Rearm::EveryCheck,
    );
    let (mut agent, _) = agent(&p, Opts::default(), vec![mine]);
    let events = collect(&mut agent, "go").await;
    assert!(guard_texts(&events).is_empty());
}

#[tokio::test]
async fn an_agent_remembers_what_fired_from_one_turn_to_the_next() {
    let p = ScriptedProvider::new([
        echo("a", serde_json::json!({})),
        text_answer("first"),
        echo("b", serde_json::json!({})),
        text_answer("second"),
    ]);
    let once = named(
        Meter::Steps,
        Mark::at(1.0),
        Action::Remind,
        Rearm::Never,
        "ONCE",
    );
    let each = named(
        Meter::Steps,
        Mark::at(1.0),
        Action::Remind,
        Rearm::NextTurn,
        "EACH",
    );
    let (mut agent, _) = agent(&p, Opts::default(), vec![once, each]);
    let first = collect(&mut agent, "one").await;
    assert_eq!(guard_texts(&first), ["[haest guard] ONCE at 1 EACH at 1"]);
    let second = collect(&mut agent, "two").await;
    assert_eq!(guard_texts(&second), ["[haest guard] EACH at 1"]);
}
