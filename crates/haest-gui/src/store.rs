//! The app's one source of truth, a GPUI entity: the haests found, the ones
//! attached (each a [`SessionModel`]), which one is shown, and the link that
//! talks to them. Every view observes it.

use std::path::PathBuf;

use gpui::{AsyncApp, Context, WeakEntity};
use haest::core::error::report;
use haest::wire::{Access, Answer, Control, Request};

use crate::{
    link::{Cmd, Event, Events, Found, Link, LinkError, Tag},
    model::{AskId, Conn, MAIN, NoticeLevel, SessionModel},
};

/// Which tab the main window shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Tab {
    /// The conversation as it happens.
    #[default]
    Stream,
    /// Main's turns as tracks.
    Timeline,
    /// The subagent tree.
    Agents,
}

/// The app's state: what was found, what is attached, what is shown.
#[derive(Debug)]
pub struct Store {
    /// Where the control sockets are.
    pub dir: PathBuf,
    /// The haests found there, oldest first; None until the first look.
    pub found: Option<Vec<Found>>,
    /// The attached ones, in the order they were attached.
    pub sessions: Vec<SessionModel>,
    /// The one the main window shows.
    pub selected: Option<String>,
    /// The agent the Agents tab shows, per session.
    pub agent: std::collections::HashMap<String, String>,
    /// The tab it shows.
    pub tab: Tab,
    /// Replaying a recording: nothing is sent anywhere.
    pub replay: bool,
    link: Link,
}

impl Store {
    /// Nothing found yet; requests go through `link`.
    pub fn new(dir: PathBuf, link: Link) -> Self {
        Self {
            dir,
            found: None,
            sessions: Vec::new(),
            selected: None,
            agent: Default::default(),
            tab: Tab::Stream,
            replay: false,
            link,
        }
    }

    /// Apply the link's events as they come, a batch per wake-up.
    pub fn listen(events: Events, cx: &mut Context<'_, Self>) {
        cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            while let Ok(first) = events.recv().await {
                let mut batch = vec![first];
                while batch.len() < 1024 {
                    match events.try_recv() {
                        Ok(ev) => batch.push(ev),
                        Err(_) => break,
                    }
                }
                let alive = this.update(cx, |store, cx| {
                    for ev in batch {
                        store.apply(ev);
                    }
                    cx.notify();
                });
                if alive.is_err() {
                    break;
                }
            }
        })
        .detach();
    }

    /// The attached haest `sid`.
    pub fn session(&self, sid: &str) -> Option<&SessionModel> {
        self.sessions.iter().find(|s| s.sid == sid)
    }

    fn session_mut(&mut self, sid: &str) -> Option<&mut SessionModel> {
        self.sessions.iter_mut().find(|s| s.sid == sid)
    }

    /// The one the main window shows, when it is attached.
    pub fn selected_session(&self) -> Option<&SessionModel> {
        self.selected.as_deref().and_then(|sid| self.session(sid))
    }

    /// The agent shown for `sid` in Agents (main unless one was picked).
    pub fn agent_of(&self, sid: &str) -> &str {
        self.agent.get(sid).map_or(MAIN, String::as_str)
    }

    /// Whether `sid` is attached.
    pub fn is_attached(&self, sid: &str) -> bool {
        self.session(sid).is_some()
    }

    /// Whether `sid` is still in the control dir.
    pub fn is_listed(&self, sid: &str) -> bool {
        self.found
            .as_ref()
            .is_some_and(|f| f.iter().any(|f| f.sid == sid))
    }

    /// One event from the link (or a replay).
    pub fn apply(&mut self, ev: Event) {
        match ev {
            Event::Sessions(found) => self.found = Some(found),
            Event::Attached { sid, found } => {
                if self.session(&sid).is_none() {
                    // A replay attaches by itself.
                    self.sessions
                        .push(SessionModel::new(sid.clone(), found.access));
                    self.selected.get_or_insert(sid.clone());
                }
                if let Some(s) = self.session_mut(&sid) {
                    s.conn = Conn::Live;
                    s.access = found.access;
                    s.pid = found.pid;
                    if s.model.is_empty() {
                        s.model = found.model;
                    }
                    if s.session.is_empty() {
                        s.session = found.session;
                    }
                }
            }
            Event::Frame { sid, frame } => {
                if let Some(s) = self.session_mut(&sid) {
                    s.apply(&frame);
                }
            }
            Event::History { sid, seq, messages } => {
                if let Some(s) = self.session_mut(&sid) {
                    s.apply_history(seq, &messages);
                }
            }
            Event::Lost { sid, why } => {
                if let Some(s) = self.session_mut(&sid) {
                    s.conn = Conn::Lost(why);
                    s.state = haest::wire::State::Idle;
                    // Nothing can answer them now; a reconnect brings back
                    // the ones the haest still waits on.
                    s.asks.clear();
                }
            }
            Event::Note { sid, text } => {
                if let Some(s) = self.session_mut(&sid) {
                    s.note(NoticeLevel::Error, text);
                }
            }
            Event::Reply { sid, tag, result } => self.replied(&sid, tag, result),
        }
    }

    fn replied(
        &mut self,
        sid: &str,
        tag: Tag,
        result: Result<Option<serde_json::Value>, LinkError>,
    ) {
        let Some(s) = self.session_mut(sid) else {
            return;
        };
        match (tag, result.map_err(|e| report(&e))) {
            (Tag::Answer(ask, _), Ok(_)) => s.answered(ask),
            (Tag::Answer(ask, _), Err(why)) => {
                s.answered(ask);
                s.note(
                    NoticeLevel::Warning,
                    format!("The answer was not taken: {why}"),
                );
            }
            (Tag::Turn, Err(why)) => {
                s.note(NoticeLevel::Error, format!("The input was refused: {why}"));
            }
            (Tag::Interrupt, Err(why)) => {
                s.note(NoticeLevel::Error, format!("Interrupt failed: {why}"));
            }
            (Tag::SetModel(model), Err(why)) => {
                s.note(
                    NoticeLevel::Error,
                    format!("Could not switch to {model}: {why}"),
                );
            }
            (Tag::SetEffort(_), Err(why)) => {
                s.note(
                    NoticeLevel::Error,
                    format!("Could not change the effort: {why}"),
                );
            }
            (Tag::SetModel(model), Ok(_)) => s.model = model,
            (Tag::SetEffort(effort), Ok(_)) => s.effort = effort,
            (Tag::Turn | Tag::Interrupt, Ok(_)) => {}
        }
    }

    /// Follow `sid` and show it.
    pub fn attach(&mut self, sid: &str) {
        if self.session(sid).is_none() {
            let access = self
                .found
                .iter()
                .flatten()
                .find(|f| f.sid == sid)
                .map_or(Access::Watch, |f| f.access);
            let mut s = SessionModel::new(sid, access);
            if let Some(f) = self.found.iter().flatten().find(|f| f.sid == sid) {
                s.model = f.model.clone();
                s.session = f.session.clone();
                s.pid = f.pid;
                s.started_ms = f.started_ms;
            }
            self.sessions.push(s);
            self.link.send(Cmd::Attach {
                sid: sid.into(),
                after: 0,
            });
        }
        self.selected = Some(sid.into());
    }

    /// Follow `sid` again after its connection was lost: its stream is
    /// rebuilt from the start, so asks it still waits on come back (frames
    /// after the last seq alone would skip them).
    pub fn reconnect(&mut self, sid: &str) {
        if let Some(s) = self.session_mut(sid) {
            let mut fresh = SessionModel::new(sid, s.access);
            fresh.model = s.model.clone();
            fresh.session = s.session.clone();
            fresh.pid = s.pid;
            fresh.started_ms = s.started_ms;
            *s = fresh;
            self.link.send(Cmd::Attach {
                sid: sid.into(),
                after: 0,
            });
        }
    }

    /// Stop following `sid`; its asks leave the queue with it.
    pub fn detach(&mut self, sid: &str) {
        self.link.send(Cmd::Detach { sid: sid.into() });
        self.sessions.retain(|s| s.sid != sid);
        if self.selected.as_deref() == Some(sid) {
            self.selected = self.sessions.last().map(|s| s.sid.clone());
        }
    }

    /// Show `sid`, attaching to it first when needed.
    pub fn select(&mut self, sid: &str) {
        if self.is_attached(sid) {
            self.selected = Some(sid.into());
        } else {
            self.attach(sid);
        }
    }

    /// Show `agent` in `sid`'s Agents tab.
    pub fn select_agent(&mut self, sid: &str, agent: &str) {
        self.agent.insert(sid.into(), agent.into());
    }

    fn command(&mut self, sid: &str, request: Request, tag: Tag) -> bool {
        let able = self.session(sid).is_some_and(SessionModel::can_command);
        if able && !self.replay {
            self.link.send(Cmd::Send {
                sid: sid.into(),
                request,
                tag,
            });
        }
        able
    }

    /// Input for the shown haest: a turn, or a part of the running one.
    pub fn send_turn(&mut self, text: String) -> bool {
        let Some(sid) = self.selected.clone() else {
            return false;
        };
        if text.trim().is_empty() {
            return false;
        }
        self.command(&sid, Request::Turn { text }, Tag::Turn)
    }

    /// Stop the shown haest's running turn.
    pub fn interrupt(&mut self) {
        if let Some(sid) = self.selected.clone() {
            self.command(&sid, Request::Control(Control::Interrupt), Tag::Interrupt);
        }
    }

    /// Switch the shown haest to `model` (`provider:model`) from its next turn.
    pub fn set_model(&mut self, model: String) {
        if let Some(sid) = self.selected.clone() {
            let request = Request::Control(Control::SetModel {
                model: model.clone(),
            });
            self.command(&sid, request, Tag::SetModel(model));
        }
    }

    /// None: the model's own default.
    pub fn set_effort(&mut self, effort: Option<String>) {
        if let Some(sid) = self.selected.clone() {
            let request = Request::Control(Control::SetEffort {
                effort: effort.clone(),
            });
            self.command(&sid, request, Tag::SetEffort(effort));
        }
    }

    /// Answer `ask` of `sid`.
    pub fn answer(&mut self, sid: &str, ask: AskId, answer: Answer) {
        let AskId::Approval(n) = ask else {
            return;
        };
        let request = Request::ApprovalAnswer { ask: n, answer };
        let replay = self.replay;
        if self.command(sid, request, Tag::Answer(ask, answer))
            && let Some(s) = self.session_mut(sid)
        {
            s.answering(ask, answer);
            // A replay has nobody to confirm it.
            if replay {
                s.answered(ask);
            }
        }
    }
}

impl SessionModel {
    /// A line in main's stream that the frames do not carry.
    pub fn note(&mut self, level: NoticeLevel, text: String) {
        if let Some(main) = self.streams.get_mut(MAIN) {
            main.notice(level, text);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::approval_queue;

    fn frame(line: &str) -> Event {
        let frame: haest::session::Frame = serde_json::from_str(line).unwrap();
        Event::Frame {
            sid: frame.sid.clone(),
            frame: Box::new(frame),
        }
    }

    fn store_with_an_ask() -> Store {
        let mut store = Store::new(PathBuf::from("/x"), Link::detached());
        let found = Found {
            sid: "s1".into(),
            session: "a".into(),
            model: "p:m".into(),
            pid: 1,
            access: Access::Command,
            started_ms: 1,
        };
        store.apply(Event::Sessions(vec![found.clone()]));
        store.apply(Event::Attached {
            sid: "s1".into(),
            found,
        });
        store.apply(frame(
            r#"{"v":1,"sid":"s1","seq":5,"ts":10,"path":["main"],"type":"approval_ask","ask":1,"toolName":"Bash","description":"d","preview":"rm -rf x","key":null}"#,
        ));
        store
    }

    // Found in the screenshots: a lost haest's ask stayed in the queue
    // ("1 waiting"), though nothing can answer it.
    #[test]
    fn a_lost_connection_takes_its_asks_out_of_the_queue() {
        let mut store = store_with_an_ask();
        assert_eq!(approval_queue(&store.sessions).len(), 1);
        store.apply(Event::Lost {
            sid: "s1".into(),
            why: "gone".into(),
        });
        assert!(approval_queue(&store.sessions).is_empty());
    }

    // And reconnecting follows the stream from its start, so an ask the
    // haest still waits on arrives again (from after the last seq it would
    // be skipped).
    #[test]
    fn reconnecting_rebuilds_the_session_so_a_pending_ask_returns() {
        let mut store = store_with_an_ask();
        store.apply(Event::Lost {
            sid: "s1".into(),
            why: "gone".into(),
        });
        store.reconnect("s1");
        let s = store.session("s1").unwrap();
        assert_eq!(
            (s.conn.clone(), s.last_seq, s.model.as_str()),
            (Conn::Connecting, 0, "p:m")
        );
        store.apply(frame(
            r#"{"v":1,"sid":"s1","seq":5,"ts":10,"path":["main"],"type":"approval_ask","ask":1,"toolName":"Bash","description":"d","preview":"rm -rf x","key":null}"#,
        ));
        assert_eq!(approval_queue(&store.sessions).len(), 1);
    }
}
