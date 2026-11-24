//! What one connection keeps: the host's conversations, what the client
//! takes, and the sessions it opened ([`Live`]), each with what its parts
//! share.

use std::{
    collections::{HashMap, HashSet},
    path::PathBuf,
    sync::{
        Arc, Mutex, PoisonError,
        atomic::{AtomicU64, Ordering},
    },
};

use agent_client_protocol::{
    Error,
    v1::schema::{ClientCapabilities, SessionId},
};
use haest_runtime::{HubUi, Runtime};
use haest_session::SessionHandle;
use tokio::{sync::watch, task::JoinHandle};

use crate::ConversationsRef;

/// A session's conversation, as the session driver runs it.
pub(crate) type Session = SessionHandle<Runtime<HubUi>>;

/// The optional updates the client said it takes.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct ClientCaps {
    /// `notice` updates (`session.notices`).
    pub(crate) notices: bool,
    /// Subagents as child sessions (`subagents`).
    pub(crate) subagents: bool,
}

impl ClientCaps {
    pub(crate) fn of(caps: &ClientCapabilities) -> Self {
        Self {
            notices: caps.session.as_ref().is_some_and(|s| s.notices.is_some()),
            subagents: caps.subagents.is_some(),
        }
    }
}

/// The connection's state, shared by every handler.
pub(crate) struct State {
    /// How a conversation is built.
    pub(crate) conversations: ConversationsRef,
    /// Where a request that names no directory looks.
    pub(crate) cwd: PathBuf,
    caps: Mutex<ClientCaps>,
    sessions: Mutex<HashMap<String, Arc<Live>>>,
    streams: AtomicU64,
}

impl State {
    pub(crate) fn new(conversations: ConversationsRef, cwd: PathBuf) -> Self {
        Self {
            conversations,
            cwd,
            caps: Mutex::default(),
            sessions: Mutex::default(),
            streams: AtomicU64::new(0),
        }
    }

    pub(crate) fn caps(&self) -> ClientCaps {
        *self.caps.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub(crate) fn set_caps(&self, caps: ClientCaps) {
        *self.caps.lock().unwrap_or_else(PoisonError::into_inner) = caps;
    }

    /// A new stream id for a session's hub: haest's frames name it, the
    /// client never sees it.
    pub(crate) fn stream_id(&self) -> String {
        format!("acp-{}", self.streams.fetch_add(1, Ordering::Relaxed) + 1)
    }

    fn sessions(&self) -> std::sync::MutexGuard<'_, HashMap<String, Arc<Live>>> {
        self.sessions.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The open session `id`; [`no_session`] when there is none.
    pub(crate) fn session(&self, id: &SessionId) -> Result<Arc<Live>, Error> {
        self.sessions()
            .get(&*id.0)
            .cloned()
            .ok_or_else(|| no_session(id))
    }

    pub(crate) fn insert(&self, live: Arc<Live>) {
        self.sessions().insert(live.id.0.to_string(), live);
    }

    pub(crate) fn remove(&self, id: &SessionId) -> Option<Arc<Live>> {
        self.sessions().remove(&*id.0)
    }

    /// Close every session still open: their conversations end, their
    /// files are saved. The client has gone.
    pub(crate) async fn close_all(&self) {
        let open: Vec<Arc<Live>> = self.sessions().drain().map(|(_, l)| l).collect();
        for live in open {
            live.close().await;
        }
    }
}

/// The error for a request on a session not open on this connection.
pub(crate) fn no_session(id: &SessionId) -> Error {
    Error::invalid_params().data(format!("no open session {}", id.0))
}

/// An open session.
pub(crate) struct Live {
    /// Its id: the haest session file's.
    pub(crate) id: SessionId,
    /// Its conversation.
    pub(crate) session: Session,
    /// How far its stream has been told the client.
    pub(crate) relay: Relay,
    /// The task telling the client its stream.
    pub(crate) forward: JoinHandle<()>,
}

impl Live {
    /// End the conversation (what it started stopped, its file saved), then
    /// the task that told its stream.
    pub(crate) async fn close(&self) {
        self.session.close().await;
        self.forward.abort();
    }

    /// Wait until the client has been told every frame the stream holds now.
    pub(crate) async fn told(&self) {
        self.relay.reached(self.session.hub().last_seq()).await;
    }
}

/// How far a session's stream has been told the client: the seq of the last
/// frame the forwarder sent on.
#[derive(Debug, Clone)]
pub(crate) struct Relay(watch::Receiver<u64>);

impl Relay {
    /// The forwarder's end, and a relay that reads it.
    pub(crate) fn channel() -> (watch::Sender<u64>, Self) {
        let (tx, rx) = watch::channel(0);
        (tx, Self(rx))
    }

    /// Wait until frame `seq` has been told the client, or the forwarder
    /// has gone (the session ended).
    pub(crate) async fn reached(&self, seq: u64) {
        let mut rx = self.0.clone();
        let _gone = rx.wait_for(|told| *told >= seq).await;
    }
}

/// The tool calls the client has been told of, by id: an approval ask on
/// one of them is shown on it.
#[derive(Debug, Clone, Default)]
pub(crate) struct Announced(Arc<Mutex<HashSet<String>>>);

impl Announced {
    pub(crate) fn insert(&self, id: &str) {
        let mut ids = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        ids.insert(id.to_string());
    }

    pub(crate) fn contains(&self, id: &str) -> bool {
        let ids = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        ids.contains(id)
    }
}
