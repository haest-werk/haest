//! Where session records go: the [`SessionSink`] seam, and the JSONL file haest
//! writes itself through an [`Fs`].

use std::{
    fmt, io,
    path::{Path, PathBuf},
};

use async_trait::async_trait;
use tokio::sync::{Mutex, mpsc, oneshot};

use super::{
    InvalidSessionId, ReplayedSession, SessionHeader, SessionRecord, replay, session_file,
    sessions_dir,
};
use crate::fs::{Fs, FsLock, FsRef, ensure_dir};

/// Where a conversation's records go. `append` is synchronous so the loop's
/// `on_message` hook can call it; records land in call order.
///
/// PLUG POINT: a host that keeps the file itself (or ships records elsewhere)
/// implements this; haest's own is [`JsonlSessionFile`]. To resume with
/// [`replay`], keep each record as one line of its JSON (`serde_json`).
#[async_trait]
pub trait SessionSink: Send + Sync {
    /// Queue or write one record. It runs on the loop's path (a host calls it
    /// from [`AgentHooks::on_message`](crate::AgentHooks::on_message)), so it
    /// must not block for long. Errors are the sink's own to report.
    fn append(&self, record: &SessionRecord);
    /// Resolves once every record appended so far has landed. Default: nothing queued.
    async fn flush(&self) {}
}

crate::plug_ref! {
    /// A [`SessionSink`], shared: what [`RunStore::sink`](crate::run::RunStore::sink)
    /// hands out.
    pub struct SessionSinkRef(dyn SessionSink);
}

enum Job {
    /// The header, and where the create's outcome goes.
    Create(String, oneshot::Sender<io::Result<()>>),
    Line(String),
    Flush(oneshot::Sender<()>),
}

/// The session file, written through a haest [`Fs`]. Writes are QUEUED and run
/// strictly in order on a background task, so callers keep a synchronous
/// `append` (history order is the file order) while the I/O stays async.
/// [`SessionSink::flush`] waits for the queue -- a host awaits it before exiting.
///
/// Must be created inside a tokio runtime (the writer task is spawned there).
/// The writer stops when the value is dropped, after what is queued.
/// [`try_lock`](Self::try_lock) holds the file for this writer alone until
/// then, so two runs never write one session.
///
/// # Examples
///
/// ```
/// use std::path::Path;
///
/// use haest_core::{
///     FsRef, MemFs, Message,
///     session::{JsonlSessionFile, SessionRecord, SessionSink},
/// };
///
/// # #[tokio::main(flavor = "current_thread")]
/// # async fn main() -> Result<(), haest_core::BoxError> {
/// let fs = FsRef::new(MemFs::new());
/// let file = JsonlSessionFile::new(fs.clone(), "/data", "s1")?;
/// file.create("conversation-1", Path::new("/project"), 0).await?;
/// file.append(&SessionRecord::Message { t: 1, message: Message::user("hi") });
/// file.flush().await;
///
/// // Later, or in another process: the same file, replayed.
/// let again = JsonlSessionFile::new(fs, "/data", "s1")?;
/// assert_eq!(again.path(), Path::new("/data/sessions/s1.jsonl"));
/// assert_eq!(again.load().await?.messages, [Message::user("hi")]);
/// # Ok(())
/// # }
/// ```
pub struct JsonlSessionFile {
    path: PathBuf,
    session_id: String,
    fs: FsRef,
    tx: mpsc::UnboundedSender<Job>,
    /// The file's lock once [`JsonlSessionFile::try_lock`] took it; None
    /// before, or where the file system has no locks.
    held: Mutex<Option<FsLock>>,
}

/// The file's path and the session's id.
impl fmt::Debug for JsonlSessionFile {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("JsonlSessionFile")
            .field("path", &self.path)
            .field("session_id", &self.session_id)
            .finish_non_exhaustive()
    }
}

impl JsonlSessionFile {
    /// The session file `<data_dir>/sessions/<session_id>.jsonl`, through
    /// `fs`; errors of queued writes are dropped. Nothing is written until
    /// [`JsonlSessionFile::create`] or the first record.
    ///
    /// # Errors
    ///
    /// [`InvalidSessionId`] unless `session_id` is `[a-zA-Z0-9_-]+`.
    ///
    /// # Panics
    ///
    /// Outside a tokio runtime.
    pub fn new(
        fs: impl Into<FsRef>,
        data_dir: impl AsRef<Path>,
        session_id: &str,
    ) -> Result<Self, InvalidSessionId> {
        Self::new_with_error_handler(fs, data_dir, session_id, |_| {})
    }

    /// Same, handing every failed queued write to `on_error` (on the writer
    /// task); later writes still run.
    ///
    /// # Errors
    ///
    /// [`InvalidSessionId`] unless `session_id` is `[a-zA-Z0-9_-]+`.
    ///
    /// # Panics
    ///
    /// Outside a tokio runtime.
    pub fn new_with_error_handler(
        fs: impl Into<FsRef>,
        data_dir: impl AsRef<Path>,
        session_id: &str,
        on_error: impl Fn(io::Error) + Send + 'static,
    ) -> Result<Self, InvalidSessionId> {
        let fs = fs.into();
        let path = session_file(&sessions_dir(data_dir.as_ref()), session_id)?;
        let (tx, rx) = mpsc::unbounded_channel();
        tokio::spawn(write_queue(fs.clone(), path.clone(), rx, on_error));
        Ok(Self {
            path,
            session_id: session_id.into(),
            fs,
            tx,
            held: Mutex::default(),
        })
    }

    /// The file's path: `<data_dir>/sessions/<session_id>.jsonl`.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The session's id, as given.
    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    /// Is the file there? Does not wait for queued writes.
    pub async fn exists(&self) -> bool {
        self.fs.exists(&self.path).await
    }

    /// Create the file with its header (0600, in a 0700 dir); no-op if it
    /// exists. Resolves once the header is on disk, in order with the records
    /// appended before it; an error here is the caller's, not `on_error`'s.
    /// `created_ms`: milliseconds since the epoch, for the header's `createdAt`.
    /// The header's ids are this file's session id and `conversation_id`
    /// ([`SessionHeader`]).
    ///
    /// # Errors
    ///
    /// The file system's, from making the dir, writing the header or setting
    /// the modes; or `Other` when the writer task has stopped.
    pub async fn create(
        &self,
        conversation_id: &str,
        cwd: &Path,
        created_ms: u64,
    ) -> io::Result<()> {
        let header = SessionRecord::from(SessionHeader {
            session_id: self.session_id.clone(),
            conversation_id: conversation_id.into(),
            cwd: cwd.into(),
            created_ms,
        });
        let (done, outcome) = oneshot::channel();
        let stopped = || io::Error::other("the session writer has stopped");
        self.tx
            .send(Job::Create(line_of(&header), done))
            .map_err(|_closed| stopped())?;
        outcome.await.map_err(|_closed| stopped())?
    }

    /// Hold the file for this writer alone until this value is dropped
    /// ([`Fs::try_lock`]): meanwhile another writer's `try_lock` of the
    /// session, in this process or another, fails with `WouldBlock`. On a
    /// file system with no locks it holds nothing, and succeeds. Once it
    /// holds the file, it returns at once.
    ///
    /// Take it before the first record of a session another writer may open:
    /// after [`create`](Self::create) for a new one, before
    /// [`load`](Self::load) for a saved one. The lock goes as this value is
    /// dropped, before its queued records land: [`flush`](SessionSink::flush)
    /// first.
    ///
    /// # Errors
    ///
    /// `WouldBlock` when another writer holds the session, `NotFound` when
    /// the file is not there yet, and the file system's other errors.
    pub async fn try_lock(&self) -> io::Result<()> {
        let mut held = self.held.lock().await;
        if held.is_none() {
            *held = self.fs.try_lock(&self.path).await?;
        }
        Ok(())
    }

    /// Flush, then replay the file. Never fails on a bad line (see [`replay`]);
    /// invalid UTF-8 from a torn write is replaced, not fatal. A file that
    /// ends mid-line (a write a crash cut off) gets that line ended before the
    /// next record, so the record is a line of its own and replays.
    ///
    /// # Errors
    ///
    /// The file system's read error: `NotFound` when there is no file yet.
    pub async fn load(&self) -> io::Result<ReplayedSession> {
        SessionSink::flush(self).await;
        let bytes = self.fs.read(&self.path).await?;
        if bytes.last().is_some_and(|&b| b != b'\n') {
            let _ = self.tx.send(Job::Line("\n".into()));
        }
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
    fs: FsRef,
    path: PathBuf,
    mut rx: mpsc::UnboundedReceiver<Job>,
    on_error: impl Fn(io::Error),
) {
    while let Some(job) = rx.recv().await {
        let res = match job {
            Job::Create(header, done) => {
                let _ = done.send(create(&*fs, &path, &header).await);
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
