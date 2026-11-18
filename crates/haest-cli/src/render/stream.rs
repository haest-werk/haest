//! The session's stream, frame by frame: in json mode each frame's line as it
//! is, otherwise what the human reads of it.

use std::io::Write;

use haest::{
    LoopEvent,
    session::{Body, Frame},
    wire::{Outbound, State, encode_line},
};

use super::{DIM, Level, Renderer};

impl<O: Write, E: Write> Renderer<O, E> {
    /// One frame of the session's stream: in json mode its line as it is;
    /// otherwise what the human reads of it. The cost line comes after a
    /// turn of the conversation's own, with the session's total from the
    /// status that follows it.
    pub(crate) fn frame(&mut self, frame: &Frame) {
        if self.json {
            let _ = self.out.write_all(encode_line(frame).as_bytes());
            let _ = self.out.flush();
            return;
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
            (Body::Wire(wire), subagent) => self.wire(wire, subagent),
        }
    }

    /// One of the wire's own frames, in text mode.
    fn wire(&mut self, frame: &Outbound, subagent: Option<&str>) {
        match (frame, subagent) {
            (Outbound::Notice { level, text }, _) => self.notice(*level, text),
            (Outbound::Ask { text }, _) => self.ask(text),
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
            // A watcher's view of an ask another peer answers.
            (
                Outbound::ApprovalAsk {
                    ask,
                    tool_name,
                    description,
                    ..
                },
                _,
            ) => self.notice(
                Level::Warning,
                &format!("{tool_name} asks for approval: {description} (ask {ask})"),
            ),
            (Outbound::ApprovalWithdrawn { ask }, _) => {
                self.note(DIM, &format!("(approval ask {ask} withdrawn)"));
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

    #[test]
    fn an_approval_ask_and_its_withdrawal_are_lines_for_a_watcher() {
        let (mut out, mut err) = (Vec::new(), Vec::new());
        {
            let mut r = Renderer::new(&mut out, &mut err, false, false);
            let ask = Outbound::ApprovalAsk {
                ask: 3,
                tool_name: "Bash".into(),
                description: "delete the build dir".into(),
                preview: "rm -rf build".into(),
                key: None,
            };
            r.frame(&wire_frame(1, ask));
            r.frame(&wire_frame(2, Outbound::ApprovalWithdrawn { ask: 3 }));
        }
        let err = String::from_utf8(err).unwrap();
        assert!(
            err.contains("Bash asks for approval: delete the build dir"),
            "{err}"
        );
        assert!(err.contains("withdrawn"), "{err}");
    }
}
