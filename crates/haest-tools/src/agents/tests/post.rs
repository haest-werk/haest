//! What a host hears and sends beyond the agent tools: the person writing to
//! a running member (and the parent told of every message), main's answers
//! apart from its messages, a hub that says when nothing is in flight, and a
//! parent's own note.

use std::time::{Duration, Instant};

use haest_core::{Message, provider::ProviderRequest};
use serde_json::json;
use tokio_util::sync::CancellationToken;

use super::{
    caps,
    lifecycle::Lifecycle,
    support::{ForMain, Heard, Host, Rig, Script, agent, call, now, say},
};
use crate::agents::{AgentError, AgentRef, MemberCaps, MemberStart, Sender, SpawnInput};

fn user_texts(req: &ProviderRequest) -> Vec<String> {
    let texts = req.messages.iter().filter_map(|m| match m {
        Message::User(u) => Some(u.content.clone()),
        _ => None,
    });
    texts.collect()
}

fn background(name: &str, prompt: &str) -> SpawnInput {
    SpawnInput {
        description: name.into(),
        name: Some(name.into()),
        prompt: prompt.into(),
        background: true,
        ..Default::default()
    }
}

/// Polls `done` for 5 s at most.
async fn until(done: impl Fn() -> bool) {
    let end = Instant::now() + Duration::from_secs(5);
    while !done() && Instant::now() < end {
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

fn person() -> Sender {
    Sender::Person("owner".into())
}

#[tokio::test(flavor = "multi_thread")]
async fn a_persons_message_reaches_a_running_member_and_fires_the_event_marked_as_from_a_person() {
    let (arrived, release) = (CancellationToken::new(), CancellationToken::new());
    // Its first call waits until the messages are in; its next answers with them.
    let sub: Script = {
        let (arrived, release) = (arrived.clone(), release.clone());
        Box::new(move |req, n| {
            let (arrived, release) = (arrived.clone(), release.clone());
            Box::pin(async move {
                if n == 0 {
                    arrived.cancel();
                    release.cancelled().await;
                    return say("looking");
                }
                say(&format!("read: {}", user_texts(&req).last().unwrap()))
            })
        })
    };
    let r = Rig::new(now(|_, _| say("ok")), sub, caps());
    let main = AgentRef::main();
    let cancel = CancellationToken::new();
    let spawn = r.hub.spawn(&main, background("looker", "look"), &cancel);
    spawn.await.unwrap();
    arrived.cancelled().await;

    r.deliver(person(), "looker", "only the docs").unwrap();
    // SendMessage from main, for contrast.
    r.deliver(agent("main"), "looker", "also the tests")
        .unwrap();
    release.cancel();
    assert!(r.hub.settled(Duration::from_secs(5)).await);

    let id = r.ids("started")[0].clone();
    let heard = |from: Sender, text: &str| Heard {
        id: id.clone(),
        from,
        to: "looker".into(),
        text: text.into(),
    };
    assert_eq!(
        r.heard(),
        [
            heard(person(), "only the docs"),
            heard(Sender::Agent("main".into()), "also the tests"),
        ]
    );
    // It read both at its next step, each under who wrote it.
    let for_main = r.for_main();
    let [ForMain::Answer(answer)] = for_main.as_slice() else {
        panic!("{for_main:?}")
    };
    let from_person = "[Message from owner, the person you work for]\n\nonly the docs";
    assert!(answer.contains(from_person), "{answer}");
    assert!(
        answer.contains("[Message from agent \"main\"]\n\nalso the tests"),
        "{answer}"
    );
    // A finished member refuses the person too, and nothing more is heard.
    let late = r.deliver(person(), "looker", "one more");
    assert!(matches!(late, Err(AgentError::Finished { .. })), "{late:?}");
    assert_eq!(r.heard().len(), 2);
}

#[tokio::test(flavor = "multi_thread")]
async fn main_gets_a_background_answer_through_submit_answer_and_a_message_through_submit() {
    let r = Rig::new(
        now(|_, _| say("ok")),
        now(|_, n| match n {
            0 => vec![call(
                "m",
                "SendMessage",
                json!({"to": "main", "message": "halfway"}),
            )],
            _ => say("done"),
        }),
        caps(),
    );
    let main = AgentRef::main();
    let cancel = CancellationToken::new();
    let spawn = r.hub.spawn(&main, background("worker", "work"), &cancel);
    spawn.await.unwrap();
    assert!(r.hub.settled(Duration::from_secs(5)).await);
    // A foreground answer is the Agent call's result: it reaches no inbox.
    let foreground = SpawnInput {
        background: false,
        ..background("quick", "p")
    };
    let out = r.hub.spawn(&main, foreground, &cancel);
    assert_eq!(out.await.unwrap().text, "done");
    r.deliver(person(), "main", "how is it going?").unwrap();

    assert_eq!(
        r.for_main(),
        [
            ForMain::Message("[Message from agent \"worker\"]\n\nhalfway".into()),
            ForMain::Answer("[Agent \"worker\" finished]\n\ndone".into()),
            ForMain::Message(
                "[Message from owner, the person you work for]\n\nhow is it going?".into()
            ),
        ]
    );
    // A message for main is keyed by its sender's id; the person's by main.
    let ids: Vec<String> = r.heard().into_iter().map(|h| h.id).collect();
    assert_eq!(ids, [r.ids("started")[0].clone(), "main".to_string()]);
}

#[tokio::test(flavor = "multi_thread")]
async fn settled_resolves_once_the_last_member_has_ended_and_its_answer_is_handed_back_and_times_out_before()
 {
    let (first, second) = (CancellationToken::new(), CancellationToken::new());
    // Each member answers when its own gate opens.
    let sub: Script = {
        let (first, second) = (first.clone(), second.clone());
        Box::new(move |req, _| {
            let gate = match user_texts(&req)[0].as_str() {
                "first" => first.clone(),
                _ => second.clone(),
            };
            Box::pin(async move {
                gate.cancelled().await;
                say("done")
            })
        })
    };
    let r = Rig::new(now(|_, _| say("ok")), sub, caps());
    assert!(r.hub.idle());
    assert!(r.hub.settled(Duration::ZERO).await, "nothing in flight");

    let main = AgentRef::main();
    for name in ["first", "second"] {
        let cancel = CancellationToken::new();
        let spawn = r.hub.spawn(&main, background(name, name), &cancel);
        spawn.await.unwrap();
    }
    assert!(!r.hub.idle());
    assert!(!r.hub.settled(Duration::from_millis(30)).await, "both run");
    let hub = r.hub.clone();
    let waiting = tokio::spawn(async move { hub.settled(Duration::MAX).await });

    first.cancel();
    until(|| r.for_main().len() == 1).await;
    assert!(
        !r.hub.settled(Duration::from_millis(30)).await,
        "the second still runs"
    );
    assert!(!waiting.is_finished());

    second.cancel();
    assert!(waiting.await.unwrap());
    assert_eq!(r.for_main().len(), 2);
    // The hub was busy while each answer was handed back: settled waits for it.
    let idle_then = r.parent.idle_at_answer.lock().unwrap().clone();
    assert_eq!(idle_then, [false, false]);
    assert_eq!(r.hub.answers_due(), 0);
    assert!(r.hub.running().is_empty());
}

/// The host's own note; it keeps haest's first line, which the test provider
/// tells a subagent's calls by.
fn own_note(m: &MemberStart) -> String {
    format!(
        "# You are a subagent\n\nThe host's note: {} for {} on {}, ${} and {} steps.",
        m.name, m.parent, m.model_spec, m.caps.usd, m.caps.steps
    )
}

#[tokio::test(flavor = "multi_thread")]
async fn a_parents_own_note_replaces_haests_and_sees_the_member_as_its_run_has_it() {
    let lifecycle = Lifecycle {
        model_spec: Some("fake:run-model".into()),
        caps: Some(MemberCaps::new(0.3, 7, Duration::from_secs(90))),
        ..Lifecycle::default()
    };
    let host = Host {
        lifecycle: Some(lifecycle.into()),
        note: Some(own_note),
        ..Host::default()
    };
    let r = Rig::with_host(now(|_, _| say("ok")), now(|_, _| say("tidy")), caps(), host);
    let input = SpawnInput {
        background: false,
        ..background("tidy", "p")
    };
    let main = AgentRef::main();
    let cancel = CancellationToken::new();
    let out = r.hub.spawn(&main, input, &cancel);
    assert_eq!(out.await.unwrap().text, "tidy");

    let systems = r.parent.provider.sub_systems.lock().unwrap().clone();
    let note = "\n\n# You are a subagent\n\nThe host's note: tidy for main on fake:run-model, $0.3 and 7 steps.";
    assert!(systems[0].ends_with(note), "{}", systems[0]);
    assert!(
        !systems[0].contains("Do the task, then end your turn"),
        "haest's note is gone"
    );
}
