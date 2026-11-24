//! The session's stream, frame by frame: in json mode each frame's line as it
//! is, otherwise what the human reads of it.

use std::io::Write;

use haest::{
    LoopEvent,
    session::{Body, Frame},
    wire::{Answer, Answerer, Outbound, State, encode_line},
};

use super::{DIM, Level, Renderer};

impl<O: Write, E: Write> Renderer<O, E> {
    /// One frame of the session's stream: in json mode its line as it is;
    /// otherwise what the human reads of it. The cost line comes after a
    /// turn of the conversation's own, with the session's total from the
    /// status that follows it.
    pub(crate) fn frame(&mut self, frame: &Frame) {
        self.after_lost_question(frame.seq);
        if self.json {
            return self.json_frame(frame);
        }
        let subagent = frame.path.segments().nth(1);
        match (&frame.body, subagent) {
            (Body::Loop(ev), None) => {
                if let LoopEvent::TurnEnd { usage, .. } = ev {
                    self.usage = Some(usage.clone());
                }
                self.event(ev);
            }
            (Body::Loop(ev), Some(id)) => {
                if let Some((level, text)) = self.agents.loop_event(id, ev) {
                    self.notice(level, &text);
                }
            }
            (Body::Wire(wire), subagent) => self.wire(frame.seq, wire, subagent),
        }
    }

    /// `frame`'s line on stdout, as the stream has it (json mode).
    pub(super) fn json_frame(&mut self, frame: &Frame) {
        let _ = self.out.write_all(encode_line(frame).as_bytes());
        let _ = self.out.flush();
        self.wrote(frame.seq);
    }

    /// One of the wire's own frames, frame `seq` of the stream, in text mode.
    fn wire(&mut self, seq: u64, frame: &Outbound, subagent: Option<&str>) {
        match (frame, subagent) {
            (Outbound::Notice { level, text }, _) => self.notice(*level, text),
            // A watcher read the approval ask's line: the terminal's
            // question on the same ask says it again.
            (
                Outbound::Ask {
                    approval: Some(_), ..
                },
                _,
            ) if self.watching => {}
            (Outbound::Ask { text, .. }, _) => self.asked(seq, text),
            (Outbound::Status(now), None) => {
                // Only another program queues input while the prompt is
                // open (the human's own line waits for it to close).
                if now.queued > self.queued && self.prompting() {
                    self.notice(
                        Level::Info,
                        "Input from another program waits: haest runs it when you press Enter.",
                    );
                }
                self.queued = now.queued;
                if now.state == State::Idle
                    && let Some(usage) = self.usage.take()
                {
                    self.usage(&usage, now.usd);
                }
            }
            // A watcher's view of an ask another peer answers. At the
            // conversation's own terminal the ask is its own question,
            // drawn as one, and given up with it.
            (
                Outbound::ApprovalAsk {
                    ask,
                    tool_name,
                    description,
                    ..
                },
                _,
            ) if self.watching => self.notice(
                Level::Warning,
                &format!("{tool_name} asks for approval: {description} (ask {ask})"),
            ),
            (Outbound::ApprovalWithdrawn { ask }, _) if self.watching => {
                self.note(DIM, &format!("(approval ask {ask} withdrawn)"));
            }
            (Outbound::ApprovalAnswered { ask, answer, by }, _) => {
                self.approval_answered(*ask, *answer, *by);
            }
            (
                Outbound::SubagentStarted {
                    name,
                    parent,
                    description,
                },
                Some(id),
            ) => {
                let (level, text) = self.agents.started(id, name, parent, description);
                self.notice(level, &text);
            }
            (Outbound::SubagentStopped { ok }, Some(id)) => {
                let (level, text) = self.agents.stopped(id, *ok);
                self.notice(level, &text);
            }
            (Outbound::Gap { missed, .. }, _) => {
                let n = missed.map_or("Some".to_string(), |n| n.to_string());
                self.note(
                    DIM,
                    &format!("({n} events not shown: the terminal fell behind)"),
                );
            }
            _ => {}
        }
    }

    /// Approval ask `ask` was answered `answer` by `by`. A watcher reads
    /// who answered; at the conversation's own terminal only a peer's
    /// answer is news (the human typed their own): it is why the question
    /// above ended.
    fn approval_answered(&mut self, ask: u64, answer: Answer, by: Answerer) {
        if self.watching {
            let who = match by {
                Answerer::Terminal => "at its terminal",
                _ => "by another program",
            };
            let line = format!("(approval ask {ask} answered {who}: {answer})");
            return self.note(DIM, &line);
        }
        if by == Answerer::Terminal {
            return;
        }
        let text = match answer {
            Answer::Allow => "Another program allowed it.",
            Answer::Always => "Another program allowed it, and the same from now on.",
            _ => "Another program denied it.",
        };
        self.notice(Level::Info, text);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render::tests::wire_frame;

    fn idle(queued: usize) -> Outbound {
        Outbound::Status(haest::wire::Snapshot {
            state: State::Idle,
            session: "x".into(),
            model_spec: "m".into(),
            effort: None,
            usd: None,
            queued,
            messages: 0,
            last_seq: 0,
        })
    }

    #[test]
    fn input_from_another_program_queued_while_the_prompt_is_open_is_announced_above_it() {
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let (tx, rx) = std::sync::mpsc::channel();
        {
            let mut r = Renderer::new(&mut out, &mut err, false, false);
            r.frame(&wire_frame(1, idle(0)));
            r.set_above(Some(tx));
            r.frame(&wire_frame(2, idle(1)));
            // Already said: the same count again says nothing.
            r.frame(&wire_frame(3, idle(1)));
            r.set_above(None);
            // Not prompting: the turn about to run shows itself.
            r.frame(&wire_frame(4, idle(2)));
        }
        let above: Vec<String> = rx.try_iter().collect();
        assert_eq!(above.len(), 1, "{above:?}");
        assert!(above[0].contains("press Enter"), "{above:?}");
        assert!(err.is_empty() && out.is_empty());
    }

    fn approval_ask(ask: u64) -> Outbound {
        Outbound::ApprovalAsk {
            ask,
            tool_name: "Bash".into(),
            tool_call_id: Some("call-1".into()),
            description: "delete the build dir".into(),
            preview: "rm -rf build".into(),
            key: None,
        }
    }

    fn answered(ask: u64, answer: Answer, by: Answerer) -> Outbound {
        Outbound::ApprovalAnswered { ask, answer, by }
    }

    #[test]
    fn an_approval_ask_and_its_ending_are_lines_for_a_watcher() {
        let (mut out, mut err) = (Vec::new(), Vec::new());
        {
            let mut r = Renderer::new(&mut out, &mut err, false, false).watching();
            r.frame(&wire_frame(1, approval_ask(3)));
            // The terminal's own question on the same ask: said already.
            let twin = Outbound::Ask {
                text: "Allow Bash? [y]es / [a]lways / [N]o: ".into(),
                approval: Some(3),
            };
            r.frame(&wire_frame(2, twin));
            r.frame(&wire_frame(3, Outbound::ApprovalWithdrawn { ask: 3 }));
            r.frame(&wire_frame(4, approval_ask(4)));
            r.frame(&wire_frame(
                5,
                answered(4, Answer::Deny, Answerer::Terminal),
            ));
            r.frame(&wire_frame(6, approval_ask(5)));
            r.frame(&wire_frame(7, answered(5, Answer::Allow, Answerer::Peer)));
        }
        let err = String::from_utf8(err).unwrap();
        assert!(
            err.contains("Bash asks for approval: delete the build dir"),
            "{err}"
        );
        assert!(!err.contains("[y]es"), "the question twice: {err}");
        assert!(err.contains("withdrawn"), "{err}");
        assert!(
            err.contains("ask 4 answered at its terminal: deny"),
            "{err}"
        );
        assert!(
            err.contains("ask 5 answered by another program: allow"),
            "{err}"
        );
    }

    /// At the conversation's own terminal an approval ask is the terminal's
    /// own question: its frames add no line, except a peer's answer, which
    /// says why the question ended.
    #[test]
    fn at_its_own_terminal_only_a_peers_answer_is_a_line() {
        let (mut out, mut err) = (Vec::new(), Vec::new());
        {
            let mut r = Renderer::new(&mut out, &mut err, false, false);
            r.frame(&wire_frame(1, approval_ask(3)));
            r.frame(&wire_frame(2, Outbound::ApprovalWithdrawn { ask: 3 }));
            r.frame(&wire_frame(3, approval_ask(4)));
            r.frame(&wire_frame(
                4,
                answered(4, Answer::Deny, Answerer::Terminal),
            ));
        }
        assert!(err.is_empty(), "{}", String::from_utf8_lossy(&err));
        {
            let mut r = Renderer::new(&mut out, &mut err, false, false);
            r.frame(&wire_frame(1, approval_ask(5)));
            r.frame(&wire_frame(2, answered(5, Answer::Deny, Answerer::Peer)));
        }
        let err = String::from_utf8(err).unwrap();
        assert!(err.contains("Another program denied it."), "{err}");
        assert!(out.is_empty());
    }
}
