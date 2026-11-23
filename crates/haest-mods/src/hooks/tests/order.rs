//! Who is asked, in what order, and what a failure does: the host's hooks
//! first, the first stop and the first continue win, a failing mod is
//! reported and the next one asked, and an interrupt is no failure.

use super::*;

#[tokio::test]
async fn a_failing_loop_event_is_reported_the_next_mod_is_asked_and_the_turn_goes_on() {
    let ui = TestUi::new();
    let log = Log::default();
    let broken = Looper {
        name: "broken",
        before: Says::Fail,
        after: Says::Fail,
        end: Says::Fail,
        log: log.clone(),
    };
    let next = Looper {
        name: "next",
        log: log.clone(),
        ..Looper::default()
    };
    let mods = mods_of(&ui, [broken, next]);
    let p = ScriptedProvider::new([text_answer("done")]);
    let mut agent = looped(&p, NoHooks, &mods, Tools::new());

    let end = agent.run("go", &mut Vec::new()).await;

    assert_eq!((end.stop_reason, end.error), (StopReason::EndTurn, None));
    let errors = ui.errors();
    assert_eq!(errors.len(), 3, "{errors:?}");
    assert!(errors.iter().all(|e| e.contains("broken")));
    let asked_next = said(&log).iter().filter(|l| l.starts_with("next")).count();
    assert_eq!(asked_next, 3);
}

#[tokio::test]
async fn the_hosts_hooks_run_first_and_a_host_that_keeps_the_turn_going_wins() {
    let log = Log::default();
    let host = Arc::new(Host {
        log: log.clone(),
        keep_going: AtomicBool::new(true),
        ..Host::default()
    });
    let looper = Looper {
        name: "m",
        log: log.clone(),
        end: Says::Continue("mod says go on"),
        ..Looper::default()
    };
    let mods = mods_of(&TestUi::new(), [looper]);
    let p = ScriptedProvider::new([text_answer("one"), text_answer("two")]);
    let mut agent = looped(&p, host, &mods, Tools::new());

    let end = agent.run("go", &mut Vec::new()).await;

    assert_eq!(end.stop_reason, StopReason::EndTurn);
    assert!(matches!(&agent.history()[2], Message::User(u) if u.content == "the host says go on"));
    let order = said(&log);
    assert_eq!(&order[..2], ["host model.before 1", "m model.before 1"]);
    // The host kept the first end going: the mod was not asked there.
    let first_end = order.iter().position(|l| l == "host turn.end").unwrap();
    assert!(!order[first_end + 1].contains("turn.end"), "{order:?}");
}

#[tokio::test]
async fn the_first_stop_and_the_first_continue_win_and_later_mods_are_not_asked() {
    let log = Log::default();
    let first = Looper {
        name: "first",
        log: log.clone(),
        before: Says::Stop("first"),
        ..Looper::default()
    };
    let second = Looper {
        name: "second",
        log: log.clone(),
        before: Says::Stop("second"),
        ..Looper::default()
    };
    let p = ScriptedProvider::new([]);
    let mods = mods_of(&TestUi::new(), [first, second]);
    let end = looped(&p, NoHooks, &mods, Tools::new())
        .run("go", &mut Vec::new())
        .await;
    assert!(end.error.unwrap_or_default().contains("first"));
    assert_eq!(said(&log), ["first model.before 1"]);

    let log = Log::default();
    let first = Looper {
        name: "first",
        log: log.clone(),
        end: Says::Continue("first"),
        ..Looper::default()
    };
    let second = Looper {
        name: "second",
        log: log.clone(),
        end: Says::Continue("second"),
        ..Looper::default()
    };
    let p = ScriptedProvider::new([text_answer("one"), text_answer("two")]);
    let mods = mods_of(&TestUi::new(), [first, second]);
    let mut agent = looped(&p, NoHooks, &mods, Tools::new());
    let _ = agent.run("go", &mut Vec::new()).await;
    assert!(matches!(&agent.history()[2], Message::User(u) if u.content == "first"));
    let ends: Vec<_> = said(&log)
        .into_iter()
        .filter(|l| l.contains("turn.end"))
        .collect();
    // First end: only `first` asked; second end: both asked, neither continues.
    assert_eq!(
        ends,
        [
            "first turn.end end_turn 2",
            "first turn.end end_turn 4",
            "second turn.end end_turn 4"
        ]
    );
}

/// Aborts the turn from its loop event, then fails.
struct AbortsThenFails;

#[async_trait]
impl Mod for AbortsThenFails {
    fn name(&self) -> &str {
        "aborts"
    }
    async fn model_before(
        &self,
        agent: &mut Agent,
        _: &CancellationToken,
        _: &ModEnv,
    ) -> Result<Step, BoxError> {
        agent.control().abort();
        Err("broke as the human interrupted".into())
    }
}

#[tokio::test]
async fn a_loop_event_that_fails_under_an_interrupt_is_not_reported() {
    let ui = TestUi::new();
    let mods = mods_of(&ui, [AbortsThenFails]);
    let p = ScriptedProvider::new([text_answer("never")]);
    let end = looped(&p, NoHooks, &mods, Tools::new())
        .run("go", &mut Vec::new())
        .await;
    assert_eq!(end.stop_reason, StopReason::Aborted);
    assert!(ui.errors().is_empty());
}
