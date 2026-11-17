//! [`UnixSocket`]: the session on a Unix socket, for programs on this
//! machine (`haest ctl`); and the client side: [`running_sessions`] lists the served
//! sessions, [`connect_session`] reaches one.
//!
//! The socket is `<dir>/<stream id>.sock`, beside it a discovery file
//! `<dir>/<stream id>.json` ([`SocketDiscovery`]). The dir is made 0700 and the
//! socket 0600: only the user who runs haest reaches it. Both files go when
//! the transport ends; a run that crashed leaves them, and [`running_sessions`]
//! removes them once nothing answers on the socket.

use std::{
    fs,
    io::Write,
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    time::Duration,
};

use haest_session::{BackendRef, Body, Frame, Subscription};
use haest_wire::{Access, Outbound};
use serde::{Deserialize, Serialize};
use tokio::{
    net::{
        UnixListener, UnixStream,
        unix::{OwnedReadHalf, OwnedWriteHalf},
    },
    task::JoinSet,
};

use crate::{
    Attached, Ended, Grant, NdjsonSink, NdjsonSource, Peer, Start, Transport, TransportError,
    link::until_closed, serve,
};

/// How long [`running_sessions`] waits for a session's `hello`.
const HELLO_WAIT: Duration = Duration::from_secs(2);

/// How long connections get to write what they hold once the session ended.
const FLUSH_GRACE: Duration = Duration::from_secs(1);

/// A connection to a session's control socket.
pub type UnixPeer = Peer<NdjsonSource<OwnedReadHalf>, NdjsonSink<OwnedWriteHalf>>;

/// The session on a Unix socket in a dir the host names. Each connection is
/// served under the same [`Grant`]; none is followed until it asks (`events`).
///
/// ```
/// use haest_core::{Agent, AgentDeps, AgentOptions, SharedDeps, ShutWorkspace, Tools, testing::ScriptedProvider};
/// use haest_session::{Hub, start};
/// use haest_transport::{Grant, Transport, UnixSocket, connect_session, running_sessions};
/// use haest_wire::{Origin, Request};
///
/// # #[tokio::main(flavor = "current_thread")]
/// # async fn main() -> Result<(), haest_core::BoxError> {
/// # let dir = std::env::temp_dir().join(format!("hx-doc-{}", std::process::id()));
/// let model = ScriptedProvider::new([]);
/// let deps = AgentDeps::new(SharedDeps::new(model.resolver(), "/", ShutWorkspace), Tools::new(), "");
/// let session = start(Agent::new(deps, AgentOptions::new("scripted:m")), Hub::new("s1"));
///
/// let attached = UnixSocket::new(&dir).attach(session, Grant::watch(Origin::Agent))?;
///
/// // Another program finds it and asks.
/// let found = running_sessions(&dir).await;
/// assert_eq!(found[0].discovery.sid, "s1");
/// let mut peer = connect_session(&dir, "s1").await?;
/// let now = peer.call(Request::Snapshot, |_| {}).await?.ok_or("a snapshot has a result")?;
/// assert_eq!(now["model"], "scripted:m");
///
/// attached.detach();
/// # let _ = std::fs::remove_dir_all(&dir);
/// # Ok(())
/// # }
/// ```
#[derive(Debug, Clone)]
pub struct UnixSocket {
    dir: PathBuf,
}

impl UnixSocket {
    /// Sockets in `dir` (made, and made 0700, when the transport attaches).
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }
}

/// What a session's discovery file says: where its socket is and what it
/// allows. The model and the session id come from the `hello` (they change).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SocketDiscovery {
    /// The stream's id; the socket and the file are named after it.
    pub sid: String,
    /// The process that serves it.
    pub pid: u32,
    /// When the session started, ms since the Unix epoch.
    pub started_ms: u64,
    /// What a connection may do.
    pub access: Access,
    /// The socket.
    pub socket: PathBuf,
}

impl Transport for UnixSocket {
    fn attach(
        self,
        backend: impl Into<BackendRef>,
        grant: Grant,
    ) -> Result<Attached, TransportError> {
        let backend: BackendRef = backend.into();
        let meta = backend.meta();
        private_dir(&self.dir)?;
        let socket = self.dir.join(format!("{}.sock", meta.sid));
        let file = self.dir.join(format!("{}.json", meta.sid));
        clear_stale(&socket)?;
        let listener = UnixListener::bind(&socket).map_err(|e| at("bind", &socket, e))?;
        // From here on, a failure removes what was made.
        let mut files = Files(vec![socket.clone()]);
        fs::set_permissions(&socket, fs::Permissions::from_mode(0o600))
            .map_err(|e| at("make private", &socket, e))?;
        let discovery = SocketDiscovery {
            sid: meta.sid,
            pid: meta.pid,
            started_ms: meta.started_ms,
            access: grant.access,
            socket,
        };
        files.0.push(file.clone());
        write_private(&file, &discovery)?;
        let closed = backend.subscribe(u64::MAX);
        Ok(Attached::new(tokio::spawn(accept(
            listener, files, backend, grant, closed,
        ))))
    }
}

/// Serve every connection until the conversation ends; then the files go.
async fn accept(
    listener: UnixListener,
    files: Files,
    backend: BackendRef,
    grant: Grant,
    closed: Subscription,
) -> Ended {
    let _files = files;
    let ended = until_closed(closed);
    tokio::pin!(ended);
    let mut conns = JoinSet::new();
    let why = loop {
        tokio::select! {
            conn = listener.accept() => match conn {
                Ok((stream, _)) => {
                    let (read, write) = stream.into_split();
                    let (source, sink) = (NdjsonSource::new(read), NdjsonSink::new(write));
                    conns.spawn(serve(backend.clone(), grant, Start::default(), source, sink));
                }
                Err(e) => break Ended::Failed(e.into()),
            },
            _ = conns.join_next(), if !conns.is_empty() => {}
            () = &mut ended => break Ended::Closed,
        }
    };
    drop(listener);
    let _ = tokio::time::timeout(FLUSH_GRACE, async {
        while conns.join_next().await.is_some() {}
    })
    .await;
    why
}

/// A session found in a control dir: its discovery file and the `hello` it
/// answered with.
#[derive(Debug, Clone)]
pub struct RunningSession {
    /// Its discovery file.
    pub discovery: SocketDiscovery,
    /// Its `hello`: the session id and the model now.
    pub hello: Frame,
}

impl RunningSession {
    /// The model now, `provider:model`.
    pub fn model_spec(&self) -> &str {
        match &self.hello.body {
            Body::Wire(Outbound::Hello { model_spec, .. }) => model_spec,
            _ => "",
        }
    }

    /// The session file's id now.
    pub fn session(&self) -> &str {
        match &self.hello.body {
            Body::Wire(Outbound::Hello { session, .. }) => session,
            _ => "",
        }
    }
}

/// The sessions served in `dir`, oldest first: each one whose socket answers.
/// A discovery file whose socket nothing answers on (its run crashed) is
/// removed, with its socket; one that does not say hello in time is skipped.
///
/// Only files the transport makes are dialed or removed: the socket must be
/// the one beside its discovery file (`<stream id>.sock`), and the
/// connection refused or the socket gone. Any other failure (too many open
/// files) skips the entry and keeps its files, and a discovery file naming
/// some other path is skipped and kept.
pub async fn running_sessions(dir: &Path) -> Vec<RunningSession> {
    let mut found = Vec::new();
    for (file, discovery) in discoveries(dir) {
        match UnixStream::connect(&discovery.socket).await {
            Err(e) => {
                let dead = matches!(
                    e.kind(),
                    std::io::ErrorKind::ConnectionRefused | std::io::ErrorKind::NotFound
                );
                if dead {
                    let _ = fs::remove_file(&discovery.socket);
                    let _ = fs::remove_file(&file);
                }
            }
            Ok(stream) => {
                let (read, write) = stream.into_split();
                let hello = Peer::start(NdjsonSource::new(read), NdjsonSink::new(write));
                if let Ok(Ok(peer)) = tokio::time::timeout(HELLO_WAIT, hello).await {
                    let hello = peer.hello().clone();
                    found.push(RunningSession { discovery, hello });
                }
            }
        }
    }
    found.sort_by_key(|r| r.discovery.started_ms);
    found
}

/// Connect to the session in `dir` whose stream id is `id`, or starts with it
/// when only one does. Its socket is the one beside its discovery file
/// (`<stream id>.sock`); a discovery file that names another is not a
/// session served here.
///
/// # Errors
///
/// [`TransportError::NoSession`] when none matches, [`TransportError::Ambiguous`]
/// when several do, or the connection failed.
pub async fn connect_session(dir: &Path, id: &str) -> Result<UnixPeer, TransportError> {
    let all = discoveries(dir);
    let exact = all.iter().find(|(_, d)| d.sid == id);
    let socket = match exact {
        Some((file, _)) => file.with_extension("sock"),
        None => {
            let mut matches = all.iter().filter(|(_, d)| d.sid.starts_with(id));
            match (matches.next(), matches.next()) {
                (Some((file, _)), None) => file.with_extension("sock"),
                (None, _) => return Err(TransportError::NoSession(id.into())),
                (Some(_), Some(_)) => return Err(TransportError::Ambiguous(id.into())),
            }
        }
    };
    let stream = UnixStream::connect(&socket)
        .await
        .map_err(|e| at("connect to", &socket, e))?;
    let (read, write) = stream.into_split();
    Peer::start(NdjsonSource::new(read), NdjsonSink::new(write)).await
}

/// Every discovery file in `dir` that reads and is the transport's own, with
/// its path: its socket is the one beside it, so what a file says is never
/// a path to dial or remove somewhere else.
fn discoveries(dir: &Path) -> Vec<(PathBuf, SocketDiscovery)> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    entries
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "json"))
        .filter_map(|p| {
            let discovery: SocketDiscovery = serde_json::from_slice(&fs::read(&p).ok()?).ok()?;
            (discovery.socket == p.with_extension("sock")).then_some((p, discovery))
        })
        .collect()
}

/// `dir`, there and the user's own (0700).
fn private_dir(dir: &Path) -> Result<(), TransportError> {
    fs::create_dir_all(dir).map_err(|e| at("create", dir, e))?;
    fs::set_permissions(dir, fs::Permissions::from_mode(0o700))
        .map_err(|e| at("make private", dir, e))
}

/// A socket left at `socket`: refused when a running session still answers
/// on it, else removed (its run crashed).
fn clear_stale(socket: &Path) -> Result<(), TransportError> {
    if fs::symlink_metadata(socket).is_err() {
        return Ok(());
    }
    if std::os::unix::net::UnixStream::connect(socket).is_ok() {
        return Err(TransportError::InUse(socket.into()));
    }
    fs::remove_file(socket).map_err(|e| at("remove", socket, e))
}

/// `value` as JSON at `path`, 0600, whole or not at all.
fn write_private(path: &Path, value: &SocketDiscovery) -> Result<(), TransportError> {
    let tmp = path.with_extension("json.tmp");
    let body = serde_json::to_vec_pretty(value).map_err(TransportError::Encode)?;
    let write = || -> std::io::Result<()> {
        let mut f = fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&tmp)?;
        f.write_all(&body)?;
        fs::rename(&tmp, path)
    };
    write().map_err(|e| at("write", path, e))
}

/// `what` (a verb) failed on `path`.
fn at(what: &'static str, path: &Path, source: std::io::Error) -> TransportError {
    TransportError::Path {
        what,
        path: path.into(),
        source,
    }
}

/// Files removed when dropped: the socket and the discovery file go when
/// the transport ends, however it ends.
struct Files(Vec<PathBuf>);

impl Drop for Files {
    fn drop(&mut self) {
        for path in &self.0 {
            let _ = fs::remove_file(path);
        }
    }
}

#[cfg(test)]
mod tests;
