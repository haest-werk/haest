//! An approval ask put to the peers and to a terminal at once
//! ([`OpenAsk`]): the first answer wins, the stream says whose it was, and
//! every ask ends in exactly one `approval_answered` or `approval_withdrawn`.

use haest_mods::{ApprovalAnswer, ApprovalRequest};
use haest_wire::{Answer, Answerer, Level, Outbound};

use super::support::until;
use crate::{Body, Frame, Hub, SessionError, SessionPrompter, Subscription};

fn request() -> ApprovalRequest {
    ApprovalRequest::new("Bash", "remove the build dir")
        .with_input_preview("rm -rf build")
        .with_tool_use_id("call-7")
}

/// The frames that end ask `ask`, in the stream up to a notice published
/// after it (so everything about the ask is in).
async fn endings(hub: &Hub, sub: &mut Subscription, ask: u64) -> Vec<Outbound> {
    hub.notice(Level::Info, "after");
    let frames = until(sub, |f| {
        matches!(f.body, Body::Wire(Outbound::Notice { .. }))
    })
    .await;
    frames
        .iter()
        .filter_map(|f: &std::sync::Arc<Frame>| match &f.body {
            Body::Wire(
                w @ (Outbound::ApprovalAnswered { ask: a, .. }
                | Outbound::ApprovalWithdrawn { ask: a }),
            ) if *a == ask => Some(w.clone()),
            _ => None,
        })
        .collect()
}

/// The ask names the call it holds up, so a peer shows it on that call.
#[tokio::test]
async fn an_ask_names_its_tool_call() {
    let hub = Hub::new("s1");
    let mut sub = hub.subscribe(0);
    let open = SessionPrompter::new(hub.clone()).open(&request());
    let frames = until(&mut sub, |f| {
        matches!(f.body, Body::Wire(Outbound::ApprovalAsk { .. }))
    })
    .await;
    let Body::Wire(Outbound::ApprovalAsk {
        ask, tool_call_id, ..
    }) = &frames.last().unwrap().body
    else {
        unreachable!()
    };
    assert_eq!(
        (*ask, tool_call_id.as_deref()),
        (open.ask(), Some("call-7"))
    );
}

/// A peer's answer ends the ask for everyone: the stream says a peer gave
/// it, and the terminal's own answer after it is not the one that counts.
#[tokio::test]
async fn a_peers_answer_wins_and_says_so() {
    let hub = Hub::new("s1");
    let mut sub = hub.subscribe(0);
    let mut open = SessionPrompter::new(hub.clone()).open(&request());
    let ask = open.ask();

    hub.answer(ask, Answer::Allow).unwrap();
    assert_eq!(open.peer_answer().await, ApprovalAnswer::Allow);
    drop(open);

    let said = Outbound::ApprovalAnswered {
        ask,
        answer: Answer::Allow,
        by: Answerer::Peer,
    };
    assert_eq!(endings(&hub, &mut sub, ask).await, [said]);
}

/// The terminal answers first: its answer counts, the stream says the
/// terminal gave it, and a peer's answer now is refused.
#[tokio::test]
async fn a_terminal_answer_wins_and_a_peers_is_refused() {
    let hub = Hub::new("s1");
    let mut sub = hub.subscribe(0);
    let open = SessionPrompter::new(hub.clone()).open(&request());
    let ask = open.ask();

    assert_eq!(
        open.answered_at_terminal(ApprovalAnswer::Deny),
        ApprovalAnswer::Deny
    );
    assert!(matches!(
        hub.answer(ask, Answer::Allow),
        Err(SessionError::NoSuchAsk { .. })
    ));

    let said = Outbound::ApprovalAnswered {
        ask,
        answer: Answer::Deny,
        by: Answerer::Terminal,
    };
    assert_eq!(endings(&hub, &mut sub, ask).await, [said]);
}

/// A peer's answer that lands while the terminal's is on its way is the
/// one that counts: what runs is what the stream said, once.
#[tokio::test]
async fn a_peers_answer_that_came_first_beats_the_terminals() {
    let hub = Hub::new("s1");
    let mut sub = hub.subscribe(0);
    let open = SessionPrompter::new(hub.clone()).open(&request());
    let ask = open.ask();

    hub.answer(ask, Answer::Deny).unwrap();
    assert_eq!(
        open.answered_at_terminal(ApprovalAnswer::AllowAlways),
        ApprovalAnswer::Deny
    );

    let said = Outbound::ApprovalAnswered {
        ask,
        answer: Answer::Deny,
        by: Answerer::Peer,
    };
    assert_eq!(endings(&hub, &mut sub, ask).await, [said]);
}

/// Given up on (the gate's timeout, an interrupt: the race is dropped), the
/// ask is withdrawn once, and nobody can answer it.
#[tokio::test]
async fn an_open_ask_dropped_unanswered_is_withdrawn() {
    let hub = Hub::new("s1");
    let mut sub = hub.subscribe(0);
    let open = SessionPrompter::new(hub.clone()).open(&request());
    let ask = open.ask();

    drop(open);
    assert!(matches!(
        hub.answer(ask, Answer::Allow),
        Err(SessionError::NoSuchAsk { .. })
    ));
    assert_eq!(
        endings(&hub, &mut sub, ask).await,
        [Outbound::ApprovalWithdrawn { ask }]
    );
}
