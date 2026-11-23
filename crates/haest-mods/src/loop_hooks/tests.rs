//! The loop events of native mods, through [`ModLoopHooks`](super::ModLoopHooks)
//! around a real agent loop: where each fires, and what a stop, a steer, a
//! continue and a compaction do. Who is asked first: order.rs.

use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};

use async_trait::async_trait;
use haest_core::{
    Agent, AgentHooks, AssistantMessage, Message, NoHooks, StopReason, Tools, TurnOutcome,
    compaction::{CompactionOptions, CompactionResult, compact},
    error::BoxError,
    testing::{ScriptedProvider, text_answer, tool_answer},
    workspace::WorkspaceMode,
};
use serde_json::json;
use tokio_util::sync::CancellationToken;

use crate::{Mod, ModEnv, ModRef, Mods, Step, TurnEnd, testing::*};

mod order;

type Log = Arc<Mutex<Vec<String>>>;

fn said(log: &Log) -> Vec<String> {
    log.lock().unwrap().clone()
}

/// What a [`Looper`] answers in one event.
#[derive(Clone, Copy, Default)]
enum Says {
    #[default]
    Nothing,
    Stop(&'static str),
    Steer(&'static str),
    /// Keep the turn going, while its history is shorter than four messages.
    Continue(&'static str),
    Fail,
}

/// A native mod that logs `<name> <event> <history length>` and answers as told.
#[derive(Default)]
struct Looper {
    name: &'static str,
    log: Log,
    before: Says,
    after: Says,
    end: Says,
}

impl Looper {
    fn note(&self, event: &str, agent: &Agent) {
        let line = format!("{} {event} {}", self.name, agent.history().len());
        self.log.lock().unwrap().push(line);
    }
}

fn step(says: Says, agent: &Agent) -> Result<Step, BoxError> {
    match says {
        Says::Stop(why) => Ok(Step::Stop(why.into())),
        Says::Steer(text) => {
            agent.control().steer(text);
            Ok(Step::Pass)
        }
        Says::Fail => Err("the mod broke".into()),
        Says::Nothing | Says::Continue(_) => Ok(Step::Pass),
    }
}

#[async_trait]
impl Mod for Looper {
    fn name(&self) -> &str {
        self.name
    }
    async fn model_before(
        &self,
        agent: &mut Agent,
        _: &CancellationToken,
        _: &ModEnv,
    ) -> Result<Step, BoxError> {
        self.note("model.before", agent);
        step(self.before, agent)
    }
    async fn model_after(
        &self,
        agent: &mut Agent,
        answer: &AssistantMessage,
        _: &CancellationToken,
        _: &ModEnv,
    ) -> Result<Step, BoxError> {
        self.note("model.after", agent);
        assert!(matches!(agent.history().last(), Some(Message::Assistant(a)) if a == answer));
        step(self.after, agent)
    }
    async fn turn_end(
        &self,
        agent: &mut Agent,
        outcome: &TurnOutcome,
        _: &CancellationToken,
        _: &ModEnv,
    ) -> Result<TurnEnd, BoxError> {
        self.note(&format!("turn.end {}", outcome.stop_reason), agent);
        match self.end {
            Says::Continue(text) if agent.history().len() < 4 => Ok(TurnEnd::Continue(text.into())),
            Says::Fail => Err("the mod broke".into()),
            _ => Ok(TurnEnd::Pass),
        }
    }
}

/// The host's own hooks: logs where it is asked, keeps the first turn end
/// going when `keep_going`, and keeps every compaction it hears of.
#[derive(Default)]
struct Host {
    log: Log,
    keep_going: AtomicBool,
    compactions: Mutex<Vec<usize>>,
}

#[async_trait]
impl AgentHooks for Host {
    fn on_compaction(&self, result: &CompactionResult) {
        self.compactions.lock().unwrap().push(result.kept);
    }
    async fn before_model_call(
        &self,
        a: &mut Agent,
        _: &CancellationToken,
    ) -> Result<(), BoxError> {
        let line = format!("host model.before {}", a.history().len());
        self.log.lock().unwrap().push(line);
        Ok(())
    }
    async fn before_turn_end(
        &self,
        _: &mut Agent,
        _: &TurnOutcome,
        _: &CancellationToken,
    ) -> Result<Option<String>, BoxError> {
        self.log.lock().unwrap().push("host turn.end".into());
        let go_on = self.keep_going.swap(false, Ordering::SeqCst);
        Ok(go_on.then(|| "the host says go on".to_string()))
    }
}

fn mods_of(ui: &Arc<TestUi>, mods: impl IntoIterator<Item = impl Into<ModRef>>) -> Mods {
    Mods::new(env(ui.clone(), WorkspaceMode::Sandbox), mods)
}

#[tokio::test]
async fn each_loop_event_fires_at_its_point_with_the_agent_as_it_then_is() {
    let log = Log::default();
    let looper = Looper {
        name: "m",
        log: log.clone(),
        ..Looper::default()
    };
    let mods = mods_of(&TestUi::new(), [looper]);
    let p = ScriptedProvider::new([
        tool_answer(&[("c1", "Echo", json!({}))]),
        text_answer("done"),
    ]);
    let echo = Probe::new("Echo");
    let mut agent = looped(&p, NoHooks, &mods, Tools::from_iter([echo.clone()]));

    let end = agent.run("go", &mut Vec::new()).await;

    assert_eq!(end.stop_reason, StopReason::EndTurn);
    // [input] -> call -> [input, answer] -> tool -> [.., result] -> call -> [.., answer]
    let expected = [
        "m model.before 1",
        "m model.after 2",
        "m model.before 3",
        "m model.after 4",
        "m turn.end end_turn 4",
    ];
    assert_eq!(said(&log), expected);
    assert_eq!(echo.ran().len(), 1);
}

#[tokio::test]
async fn a_stop_before_the_model_call_ends_the_turn_with_no_call_made() {
    let looper = Looper {
        name: "budget",
        before: Says::Stop("over budget"),
        ..Looper::default()
    };
    let mods = mods_of(&TestUi::new(), [looper]);
    let p = ScriptedProvider::new([text_answer("never")]);
    let mut agent = looped(&p, NoHooks, &mods, Tools::new());

    let end = agent.run("go", &mut Vec::new()).await;

    assert_eq!(end.stop_reason, StopReason::Error);
    let error = end.error.unwrap_or_default();
    assert!(
        error.contains("over budget") && error.contains("budget"),
        "{error}"
    );
    assert!(p.requests().is_empty());
}

#[tokio::test]
async fn a_stop_after_the_answer_ends_the_turn_and_its_calls_do_not_run() {
    let looper = Looper {
        name: "guard",
        after: Says::Stop("no tools today"),
        ..Looper::default()
    };
    let mods = mods_of(&TestUi::new(), [looper]);
    let p = ScriptedProvider::new([tool_answer(&[("c1", "Echo", json!({}))])]);
    let echo = Probe::new("Echo");
    let mut agent = looped(&p, NoHooks, &mods, Tools::from_iter([echo.clone()]));

    let end = agent.run("go", &mut Vec::new()).await;

    assert_eq!(end.stop_reason, StopReason::Error);
    assert!(echo.ran().is_empty());
    assert!(matches!(&agent.history()[2], Message::Tool(r) if r.is_error));
}

#[tokio::test]
async fn a_steer_after_an_answer_and_a_continue_at_the_end_keep_the_turn_going() {
    let steering = Looper {
        name: "s",
        after: Says::Steer("also check the docs"),
        ..Looper::default()
    };
    let p = ScriptedProvider::new([text_answer("first"), text_answer("second")]);
    let mods = mods_of(&TestUi::new(), [steering]);
    let mut agent = looped(&p, NoHooks, &mods, Tools::new());
    let _ = agent.run("go", &mut Vec::new()).await;
    assert!(matches!(&agent.history()[2], Message::User(u) if u.content == "also check the docs"));

    let going = Looper {
        name: "c",
        end: Says::Continue("not done yet"),
        ..Looper::default()
    };
    let p = ScriptedProvider::new([text_answer("first"), text_answer("second")]);
    let mods = mods_of(&TestUi::new(), [going]);
    let mut agent = looped(&p, NoHooks, &mods, Tools::new());
    let end = agent.run("go", &mut Vec::new()).await;
    assert_eq!(
        (end.stop_reason, p.requests().len()),
        (StopReason::EndTurn, 2)
    );
    assert!(matches!(&agent.history()[2], Message::User(u) if u.content == "not done yet"));
}

/// Compacts before a model call once the history is long.
struct Compacts;

#[async_trait]
impl Mod for Compacts {
    fn name(&self) -> &str {
        "compacts"
    }
    async fn model_before(
        &self,
        agent: &mut Agent,
        cancel: &CancellationToken,
        _: &ModEnv,
    ) -> Result<Step, BoxError> {
        if agent.history().len() > 4 {
            let cfg = CompactionOptions {
                keep_recent: 2,
                ..CompactionOptions::new(1_000)
            };
            compact(agent, cancel, &cfg).await?;
        }
        Ok(Step::Pass)
    }
}

#[tokio::test]
async fn a_rust_mod_compacts_through_the_agent_and_the_host_hears_of_it() {
    let host = Arc::new(Host::default());
    let mods = mods_of(&TestUi::new(), [Compacts]);
    let p = ScriptedProvider::new([text_answer("SUMMARY"), text_answer("done")]);
    let mut agent = looped(&p, host.clone(), &mods, Tools::new());
    let earlier = ["a", "b"].map(|t| [Message::user(t), Message::Assistant(text_answer(t))]);
    agent.replace_history(earlier.concat());

    let end = agent.run("c", &mut Vec::new()).await;

    assert_eq!(end.error, None);
    let turn_call = &p.requests()[1];
    assert!(matches!(&turn_call.messages[0], Message::User(u) if u.content.contains("SUMMARY")));
    assert_eq!(*host.compactions.lock().unwrap(), [2]);
}
