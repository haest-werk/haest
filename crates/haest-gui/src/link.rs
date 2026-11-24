//! The tokio side: finding the haests with a control socket, following an
//! attached one's stream, asking it things. Exactly what `haest ctl` does,
//! with haest's own client (`running_sessions`, `connect_session`).
//!
//! It runs on its own thread with its own tokio runtime and speaks to the UI
//! through two channels: [`Cmd`]s in, [`Event`]s out.
//!
//! Each attached haest gets one following connection (`events {after,
//! follow}`). Every request (a turn, a control verb, an approval answer, a
//! `history` after a gap) gets a connection of its own, as `haest ctl` does:
//! a turn's reply comes only when the turn ends, and an interrupt must not
//! wait behind it.

use std::{collections::HashMap, path::PathBuf, sync::Arc, time::Duration};

use haest::core::{error::report, messages::Message};
use haest::session::{Body, Frame};
use haest::transport::{TransportError, connect_session, running_sessions};
use haest::wire::{Access, Answer, Outbound, Request};
use serde_json::Value;
use thiserror::Error;
use tokio::{sync::mpsc, task::JoinHandle};

use crate::model::AskId;

/// How often the control dir is looked at again.
const DISCOVER_EVERY: Duration = Duration::from_millis(1500);
/// How long a request that is not a turn may take.
const ANSWER_WAIT: Duration = Duration::from_secs(10);

/// A haest found in the control dir, as its discovery file and `hello` say.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Found {
    /// The stream id: the socket's name, what a request names.
    pub sid: String,
    /// The conversation's session id.
    pub session: String,
    /// The model it runs, as `provider:model`.
    pub model: String,
    /// The haest's process id.
    pub pid: u32,
    /// What its socket lets a peer do: watch, or command too.
    pub access: Access,
    /// When it started, ms since the epoch (0 when only `hello` said it).
    pub started_ms: u64,
}

/// What a request was, so its reply lands in the right place.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Tag {
    /// Input: a turn, or a part of the running one.
    Turn,
    /// Stop the running turn.
    Interrupt,
    /// Switch to this model.
    SetModel(String),
    /// Change the effort; `None` is the model's own default.
    SetEffort(Option<String>),
    /// Answer this approval ask so.
    Answer(AskId, Answer),
}

/// Why a request to a haest got no reply. Show it with
/// [`report`](haest::core::error::report): a cause is its source.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum LinkError {
    /// The haest said no, in its own words.
    #[error("{why}")]
    Refused {
        /// What the haest said.
        why: String,
    },
    /// The socket could not be reached, or failed mid-request.
    #[error("the haest's socket failed")]
    Transport {
        /// What failed.
        #[source]
        source: TransportError,
    },
    /// A request that is not a turn got no answer in time.
    #[error("no answer in {secs} s")]
    NoAnswer {
        /// How long it waited.
        secs: u64,
    },
    /// `history`'s answer has no `seq`.
    #[error("history has no seq")]
    HistoryWithoutSeq,
    /// `history`'s messages are not haest's messages.
    #[error("history's messages do not read")]
    HistoryMessages {
        /// Why they do not read.
        #[source]
        source: serde_json::Error,
    },
}

impl From<TransportError> for LinkError {
    fn from(e: TransportError) -> Self {
        match e {
            TransportError::Refused(why) => Self::Refused { why },
            source => Self::Transport { source },
        }
    }
}

/// What the link tells the UI.
#[derive(Debug)]
pub enum Event {
    /// The haests the control dir holds now, oldest first.
    Sessions(Vec<Found>),
    /// The following connection said hello.
    Attached {
        /// Which haest.
        sid: String,
        /// What its `hello` says about it.
        found: Found,
    },
    /// One frame of its stream.
    Frame {
        /// Which haest.
        sid: String,
        /// The frame.
        frame: Box<Frame>,
    },
    /// `history`'s answer after a gap: messages current to `seq`.
    History {
        /// Which haest.
        sid: String,
        /// The last frame the messages include.
        seq: u64,
        /// The conversation's messages.
        messages: Vec<Message>,
    },
    /// The following connection ended, and why.
    Lost {
        /// Which haest.
        sid: String,
        /// Why, in words for the window.
        why: String,
    },
    /// Something went wrong that the stream itself does not say.
    Note {
        /// Which haest.
        sid: String,
        /// What went wrong, in words for the window.
        text: String,
    },
    /// A request's reply.
    Reply {
        /// Which haest.
        sid: String,
        /// What the request was.
        tag: Tag,
        /// Its reply's value, or why there is none.
        result: Result<Option<Value>, LinkError>,
    },
}

/// What the UI asks the link.
#[derive(Debug)]
pub enum Cmd {
    /// Follow `sid`'s stream from after `after`.
    Attach {
        /// Which haest.
        sid: String,
        /// The last frame already seen (0: from the start).
        after: u64,
    },
    /// Stop following it.
    Detach {
        /// Which haest.
        sid: String,
    },
    /// Send it `request`.
    Send {
        /// Which haest.
        sid: String,
        /// The request.
        request: Request,
        /// What it is, so its reply lands in the right place.
        tag: Tag,
    },
}

/// The UI's handle on the link.
#[derive(Debug, Clone)]
pub struct Link {
    tx: mpsc::UnboundedSender<Cmd>,
}

/// The UI's end of the event channel.
pub type Events = async_channel::Receiver<Event>;

impl Link {
    /// The link on `runtime`, watching `dir` (relative to the working
    /// directory, or not).
    pub fn start(runtime: &tokio::runtime::Handle, dir: PathBuf) -> (Self, Events) {
        let (tx, rx) = mpsc::unbounded_channel();
        let (events_tx, events) = async_channel::unbounded();
        runtime.spawn(run(Arc::new(dir), rx, events_tx));
        (Self { tx }, events)
    }

    /// A link whose commands go nowhere: replay and snapshots.
    pub fn detached() -> Self {
        let (tx, _rx) = mpsc::unbounded_channel();
        Self { tx }
    }

    /// Hand `cmd` to the link (a link that stopped drops it).
    pub fn send(&self, cmd: Cmd) {
        let _ = self.tx.send(cmd);
    }
}

async fn run(
    dir: Arc<PathBuf>,
    mut cmds: mpsc::UnboundedReceiver<Cmd>,
    events: async_channel::Sender<Event>,
) {
    let discovering = tokio::spawn(discover(dir.clone(), events.clone()));
    let mut follows: HashMap<String, JoinHandle<()>> = HashMap::new();
    while let Some(cmd) = cmds.recv().await {
        match cmd {
            Cmd::Attach { sid, after } => {
                if let Some(old) = follows.remove(&sid) {
                    old.abort();
                }
                let task = tokio::spawn(follow(dir.clone(), sid.clone(), after, events.clone()));
                follows.insert(sid, task);
            }
            Cmd::Detach { sid } => {
                if let Some(task) = follows.remove(&sid) {
                    task.abort();
                }
            }
            Cmd::Send { sid, request, tag } => {
                tokio::spawn(ask(dir.clone(), sid, request, tag, events.clone()));
            }
        }
    }
    discovering.abort();
    follows.values().for_each(JoinHandle::abort);
}

/// The control dir, looked at again and again; a change is an event.
async fn discover(dir: Arc<PathBuf>, events: async_channel::Sender<Event>) {
    let mut last: Option<Vec<Found>> = None;
    loop {
        let found: Vec<Found> = running_sessions(&dir)
            .await
            .iter()
            .map(|r| Found {
                sid: r.discovery.sid.clone(),
                session: r.session().to_string(),
                model: r.model_spec().to_string(),
                pid: r.discovery.pid,
                access: r.discovery.access,
                started_ms: r.discovery.started_ms,
            })
            .collect();
        if last.as_ref() != Some(&found) {
            last = Some(found.clone());
            if events.send(Event::Sessions(found)).await.is_err() {
                return;
            }
        }
        tokio::time::sleep(DISCOVER_EVERY).await;
    }
}

/// `sid`'s stream after `after`, followed until it ends. A `gap` is
/// followed by a `history` on a second connection; the frames that arrive
/// meanwhile wait in the socket, so they come after it.
async fn follow(dir: Arc<PathBuf>, sid: String, after: u64, events: async_channel::Sender<Event>) {
    let lost = |why: String| Event::Lost {
        sid: sid.clone(),
        why,
    };
    let mut peer = match connect_session(&dir, &sid).await {
        Ok(peer) => peer,
        Err(e) => {
            let _ = events.send(lost(why_not(&e))).await;
            return;
        }
    };
    if let Some(found) = from_hello(&sid, peer.hello()) {
        let attached = Event::Attached {
            sid: sid.clone(),
            found,
        };
        if events.send(attached).await.is_err() {
            return;
        }
    }
    let asked = Request::Events {
        after,
        follow: true,
    };
    let events_id = match peer.send(asked).await {
        Ok(id) => id,
        Err(e) => {
            let _ = events.send(lost(why_not(&e))).await;
            return;
        }
    };
    loop {
        let frame = match peer.recv().await {
            None => {
                let _ = events
                    .send(lost("the haest closed its socket".into()))
                    .await;
                return;
            }
            Some(Err(TransportError::NotAFrame(_))) => continue,
            Some(Err(e)) => {
                let _ = events.send(lost(why_not(&e))).await;
                return;
            }
            Some(Ok(frame)) => frame,
        };
        match &frame.body {
            Body::Wire(Outbound::Hello { .. }) => continue,
            Body::Wire(Outbound::Reply { id, ok, error, .. }) => {
                if *id == events_id && !ok {
                    let why = error.clone().unwrap_or_else(|| "refused".into());
                    let _ = events
                        .send(lost(format!("the stream was refused: {why}")))
                        .await;
                    return;
                }
                continue;
            }
            Body::Wire(Outbound::Gap { .. }) => {
                let gap = Event::Frame {
                    sid: sid.clone(),
                    frame: Box::new(frame),
                };
                if events.send(gap).await.is_err() {
                    return;
                }
                let caught_up = match history(&dir, &sid).await {
                    Ok((seq, messages)) => Event::History {
                        sid: sid.clone(),
                        seq,
                        messages,
                    },
                    Err(e) => Event::Note {
                        sid: sid.clone(),
                        text: format!("Could not catch up after the lost frames: {}", report(&e)),
                    },
                };
                if events.send(caught_up).await.is_err() {
                    return;
                }
            }
            _ => {
                let ev = Event::Frame {
                    sid: sid.clone(),
                    frame: Box::new(frame),
                };
                if events.send(ev).await.is_err() {
                    return;
                }
            }
        }
    }
}

/// `history` on a connection of its own: the messages and their seq.
async fn history(dir: &std::path::Path, sid: &str) -> Result<(u64, Vec<Message>), LinkError> {
    let mut peer = connect_session(dir, sid).await?;
    let reply = tokio::time::timeout(ANSWER_WAIT, peer.call(Request::History, |_| {}))
        .await
        .map_err(|_elapsed| no_answer())??
        .unwrap_or_default();
    parse_history(&reply)
}

/// `{"seq": n, "messages": [...]}`.
fn parse_history(reply: &Value) -> Result<(u64, Vec<Message>), LinkError> {
    let seq = reply["seq"].as_u64().ok_or(LinkError::HistoryWithoutSeq)?;
    let messages = serde_json::from_value(reply["messages"].clone())
        .map_err(|source| LinkError::HistoryMessages { source })?;
    Ok((seq, messages))
}

/// The wait for a reply ran out.
fn no_answer() -> LinkError {
    LinkError::NoAnswer {
        secs: ANSWER_WAIT.as_secs(),
    }
}

/// One request on a connection of its own, and its reply.
async fn ask(
    dir: Arc<PathBuf>,
    sid: String,
    request: Request,
    tag: Tag,
    events: async_channel::Sender<Event>,
) {
    let turn = matches!(request, Request::Turn { .. });
    let result = async {
        let mut peer = connect_session(&dir, &sid).await?;
        let call = peer.call(request, |_| {});
        // A turn's reply comes when the turn ends: no deadline for it.
        let reply = if turn {
            call.await
        } else {
            tokio::time::timeout(ANSWER_WAIT, call)
                .await
                .map_err(|_elapsed| no_answer())?
        };
        Ok(reply?)
    }
    .await;
    let _ = events.send(Event::Reply { sid, tag, result }).await;
}

fn from_hello(sid: &str, hello: &Frame) -> Option<Found> {
    match &hello.body {
        Body::Wire(Outbound::Hello {
            session,
            model_spec,
            pid,
            access,
            ..
        }) => Some(Found {
            sid: sid.to_string(),
            session: session.clone(),
            model: model_spec.clone(),
            pid: *pid,
            access: *access,
            started_ms: 0,
        }),
        _ => None,
    }
}

fn why_not(e: &TransportError) -> String {
    match e {
        TransportError::Refused(why) => why.clone(),
        e => report(e),
    }
}

/// The control dir `haest ctl` uses: `--control-dir`, else what haest's own
/// settings say (`control_dir`, else `<data_dir>/control`), loaded exactly
/// as the `haest` command loads them.
pub async fn control_dir(flag: Option<PathBuf>) -> PathBuf {
    if let Some(dir) = flag {
        return dir;
    }
    let env: Vec<(String, String)> = std::env::vars().collect();
    let home = std::env::var("HOME").unwrap_or_else(|_| "/".into());
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/"));
    let defaults = haest::settings::Settings::bare(cwd, &home);
    let no_flags: [(&str, &str); 0] = [];
    match haest::config::Loader::standard(defaults, env, no_flags)
        .load(&haest::workspace::RealFs)
        .await
    {
        Ok(loaded) => loaded.config.control_dir(),
        // A config that does not load: the default place.
        Err(_) => PathBuf::from(home).join(".local/share/haest/control"),
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use tokio::{io::AsyncWriteExt as _, net::UnixListener};

    use super::*;

    /// A haest as discovery sees it: a discovery file beside a socket that
    /// says `hello` to whoever connects. `dir` is absolute.
    fn fake_haest(dir: &Path, sid: &str) -> tokio::task::JoinHandle<()> {
        std::fs::create_dir_all(dir).unwrap();
        let socket = dir.join(format!("{sid}.sock"));
        let _ = std::fs::remove_file(&socket);
        let discovery = serde_json::json!({
            "sid": sid, "pid": 1, "startedMs": 1, "access": "watch", "socket": socket,
        });
        std::fs::write(dir.join(format!("{sid}.json")), discovery.to_string()).unwrap();
        let listener = UnixListener::bind(&socket).unwrap();
        let hello = format!(
            "{}\n",
            serde_json::json!({
                "v": 1, "sid": sid, "seq": 0, "ts": 1, "path": ["main"], "type": "hello",
                "session": "s", "model": "p:m", "pid": 1, "access": "watch",
            })
        );
        tokio::spawn(async move {
            while let Ok((mut stream, _)) = listener.accept().await {
                let _ = stream.write_all(hello.as_bytes()).await;
                tokio::spawn(async move {
                    tokio::time::sleep(Duration::from_secs(5)).await;
                    drop(stream);
                });
            }
        })
    }

    /// `abs` as a path relative to the working directory: `../..` up to the
    /// root, then `abs` below it.
    fn relative(abs: &Path) -> PathBuf {
        let cwd = std::env::current_dir().unwrap();
        let ups = cwd.components().count() - 1;
        let mut rel: PathBuf = std::iter::repeat_n("..", ups).collect();
        rel.push(abs.strip_prefix("/").unwrap());
        rel
    }

    // Found by the live test: a relative `--control-dir` found no
    // haest, because haest kept a discovery file only when its socket path
    // equalled `<dir>/<sid>.sock` as the reader spelled the dir. Fixed in
    // haest-transport (running_sessions); this keeps haest-gui honest about it.
    // The dir is under /tmp: a socket's path has to fit in 104 bytes.
    #[tokio::test]
    async fn a_relative_control_dir_finds_the_haests_in_it() {
        let abs = PathBuf::from(format!("/tmp/haest-gui-rel-{}", std::process::id()));
        let dir = relative(&abs);
        let _ = std::fs::remove_dir_all(&dir);
        let server = fake_haest(&abs, "rel-1");
        let (_link, events) = Link::start(&tokio::runtime::Handle::current(), dir.clone());
        let first = tokio::time::timeout(Duration::from_secs(5), events.recv())
            .await
            .expect("discovery answers")
            .expect("the link runs");
        server.abort();
        let _ = std::fs::remove_dir_all(&dir);
        let Event::Sessions(found) = first else {
            panic!("the first event is the sessions found, not {first:?}");
        };
        assert_eq!(
            found.iter().map(|f| f.sid.as_str()).collect::<Vec<_>>(),
            ["rel-1"]
        );
    }
}
