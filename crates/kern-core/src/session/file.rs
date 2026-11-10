//! Where session records go: the [`SessionSink`] seam, and the JSONL file kern
//! writes itself through an [`Fs`].

use std::{
    io,
    path::{Path, PathBuf},
    sync::Arc,
};

use async_trait::async_trait;
use tokio::sync::{mpsc, oneshot};

use super::{InvalidSessionId, ReplayedSession, SessionRecord, replay, session_path};
use crate::fs::{Fs, ensure_dir};

/// Where a conversation's records go. `append` is synchronous so the loop's
/// `on_message` hook can call it; records land in call order.
///
/// PLUG POINT: a host that keeps the file itself (or ships records elsewhere)
/// implements this; kern's own is [`JsonlSessionFile`].
#[async_trait]
pub trait SessionSink: Send + Sync {
    fn append(&self, record: &SessionRecord);
    /// Resolves once every record appended so far has landed. Default: nothing queued.
    async fn flush(&self) {}
}

enum Job {
    /// The header, and where the create's outcome goes.
    Create(String, oneshot::Sender<io::Result<()>>),
    Line(String),
    Flush(oneshot::Sender<()>),
}

/// The session file, written through a kern [`Fs`]. Writes are QUEUED and run
/// strictly in order on a background task, so callers keep a synchronous
/// `append` (history order is the file order) while the I/O stays async.
/// [`SessionSink::flush`] waits for the queue -- a host awaits it before exiting.
///
/// Must be created inside a tokio runtime (the writer task is spawned there).
pub struct JsonlSessionFile {
    path: PathBuf,
    session_id: String,
    fs: Arc<dyn Fs>,
    tx: mpsc::UnboundedSender<Job>,
}

impl JsonlSessionFile {
    /// `<data_dir>/sessions/<session_id>.jsonl`; write errors are dropped.
    pub fn new(
        fs: Arc<dyn Fs>,
        data_dir: &Path,
        session_id: &str,
    ) -> Result<Self, InvalidSessionId> {
        Self::with_error_handler(fs, data_dir, session_id, |_| {})
    }

    /// Same, handing every failed write to `on_error`; later writes still run.
    pub fn with_error_handler(
        fs: Arc<dyn Fs>,
        data_dir: &Path,
        session_id: &str,
        on_error: impl Fn(io::Error) + Send + 'static,
    ) -> Result<Self, InvalidSessionId> {
        let path = session_path(data_dir, session_id)?;
        let (tx, rx) = mpsc::unbounded_channel();
        tokio::spawn(write_queue(fs.clone(), path.clone(), rx, on_error));
        Ok(Self {
            path,
            session_id: session_id.into(),
            fs,
            tx,
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    pub async fn exists(&self) -> bool {
        self.fs.exists(&self.path).await
    }

    /// Create the file with its header (0600, in a 0700 dir); no-op if it
    /// exists. Resolves once the header is on disk, in order with the records
    /// appended before it; an error here is the caller's, not `on_error`'s.
    /// `created_ms`: milliseconds since the epoch, for the header's `createdAt`.
    pub async fn create(
        &self,
        conversation_id: &str,
        cwd: &str,
        created_ms: u64,
    ) -> io::Result<()> {
        let header = SessionRecord::header(&self.session_id, conversation_id, cwd, created_ms);
        let (done, outcome) = oneshot::channel();
        let stopped = || io::Error::other("the session writer has stopped");
        self.tx
            .send(Job::Create(line_of(&header), done))
            .map_err(|_| stopped())?;
        outcome.await.map_err(|_| stopped())?
    }

    /// Flush, then replay the file. Never fails on a bad line (see [`replay`]);
    /// invalid UTF-8 from a torn write is replaced, not fatal.
    pub async fn load(&self) -> io::Result<ReplayedSession> {
        SessionSink::flush(self).await;
        let bytes = self.fs.read(&self.path).await?;
        Ok(replay(&String::from_utf8_lossy(&bytes)))
    }
}

#[async_trait]
impl SessionSink for JsonlSessionFile {
    fn append(&self, record: &SessionRecord) {
        let _ = self.tx.send(Job::Line(line_of(record)));
    }

    async fn flush(&self) {
        let (done, wait) = oneshot::channel();
        if self.tx.send(Job::Flush(done)).is_ok() {
            let _ = wait.await;
        }
    }
}

fn line_of(record: &SessionRecord) -> String {
    let json = serde_json::to_string(record).expect("a session record always serializes");
    format!("{json}\n")
}

async fn write_queue(
    fs: Arc<dyn Fs>,
    path: PathBuf,
    mut rx: mpsc::UnboundedReceiver<Job>,
    on_error: impl Fn(io::Error),
) {
    while let Some(job) = rx.recv().await {
        let res = match job {
            Job::Create(header, done) => {
                let _ = done.send(create(fs.as_ref(), &path, &header).await);
                Ok(())
            }
            Job::Line(line) => fs.append(&path, line.as_bytes()).await,
            Job::Flush(done) => {
                let _ = done.send(());
                Ok(())
            }
        };
        if let Err(e) = res {
            on_error(e);
        }
    }
}

async fn create(fs: &dyn Fs, path: &Path, header: &str) -> io::Result<()> {
    if fs.exists(path).await {
        return Ok(());
    }
    let dir = path.parent().unwrap_or(Path::new("/"));
    ensure_dir(fs, dir).await?;
    fs.chmod(dir, 0o700).await?;
    fs.write(path, header.as_bytes()).await?;
    fs.chmod(path, 0o600).await
}
