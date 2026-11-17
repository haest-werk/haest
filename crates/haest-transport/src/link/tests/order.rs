//! A peer's requests reach the session in the order it sent them, as a
//! terminal's commands take effect in the order they were typed. Each runs
//! on a task of its own, and a runtime of several threads runs the newest
//! task first more often than not, so these run there, 20 connections each.

use super::*;

/// A `clear` written before a `turn` never clears that turn.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_clear_sent_before_a_turn_never_clears_that_turn() {
    for run in 0..20 {
        let (s, _) = session(&answers(&["one"]), hub());
        let mut peer = Peer::attach(s.clone(), Grant::command(Origin::Human), Start::default());
        // One write, as a parent that queues its work sends it.
        let lines = format!("{}\n{}", control("c", "clear"), turn("t", "hi"));
        peer.send_raw(&lines).await;
        peer.end_input().await;
        let _ = peer.rest().await;

        let messages = s.history().await.unwrap().messages;
        assert_eq!(messages.len(), 2, "run {run}: {messages:#?}");
        s.close().await;
    }
}

/// An `interrupt` written right after a `turn` stops that turn: it never
/// reaches the session first, between turns, where it does nothing.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_interrupt_sent_right_after_a_turn_stops_that_turn() {
    for run in 0..20 {
        // The turn waits in its tool until it is stopped.
        let model = ScriptedProvider::new([
            tool_answer(&[("c1", "Wait", json!({}))]),
            text_answer("never"),
        ]);
        let (s, _) = session(&model, hub());
        let mut peer = Peer::attach(s.clone(), Grant::command(Origin::Human), Start::default());
        let lines = format!("{}\n{}", turn("t", "wait"), control("stop", "interrupt"));
        peer.send_raw(&lines).await;

        let frames = tokio::time::timeout(Duration::from_secs(5), peer.reply("t")).await;
        let frames = frames.unwrap_or_else(|_| panic!("run {run}: the turn ran on"));
        let reply = frames.last().unwrap();
        assert_eq!(
            reply["result"]["stopReason"], "aborted",
            "run {run}: {reply}"
        );
        s.close().await;
    }
}
