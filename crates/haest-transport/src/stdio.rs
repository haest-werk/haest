//! [`Stdio`]: the session over a pipe pair, the way a parent process drives a
//! child: requests in on the child's stdin, frames out on its stdout.

use std::{
    io::Read,
    pin::Pin,
    task::{Context, Poll, ready},
};

use haest_session::BackendRef;
use tokio::{
    io::{AsyncRead, AsyncWrite, ReadBuf},
    sync::mpsc,
};

use crate::{
    Attached, Grant, Start, Transport, TransportError,
    link::serve,
    ndjson::{NdjsonSink, NdjsonSource},
};

/// The session over one pipe pair, NDJSON both ways. One peer: the process
/// on the other end. It follows the stream from the start (it sees what the
/// build said), and the connection ends when its input ends, after what it
/// asked for has finished, or when a `close` request ends the conversation.
///
/// [`Stdio::process`] is this process's own stdin and stdout; any other
/// pair works the same (a test's in-memory pipes).
///
/// ```
/// use haest_core::{Agent, AgentDeps, AgentOptions, SharedDeps, ShutWorkspace, Tools, testing::{ScriptedProvider, text_answer}};
/// use haest_session::{Hub, start};
/// use haest_transport::{Grant, Stdio, Transport};
/// use haest_wire::{LineDecoder, Origin};
/// use tokio::io::{AsyncReadExt, AsyncWriteExt};
///
/// # #[tokio::main(flavor = "current_thread")]
/// # async fn main() -> Result<(), haest_core::BoxError> {
/// let model = ScriptedProvider::new([text_answer("hi")]);
/// let deps = AgentDeps::new(SharedDeps::new(model.resolver(), "/", ShutWorkspace), Tools::new(), "");
/// let session = start(Agent::new(deps, AgentOptions::new("scripted:m")), Hub::new("s1"));
///
/// // The parent's ends of the two pipes.
/// let (mut to_child, child_in) = tokio::io::duplex(1 << 16);
/// let (child_out, mut from_child) = tokio::io::duplex(1 << 16);
/// let attached = Stdio::new(child_in, child_out).attach(session, Grant::command(Origin::Human))?;
///
/// to_child.write_all(b"{\"v\":1,\"id\":\"r1\",\"type\":\"turn\",\"text\":\"say hi\"}\n").await?;
/// drop(to_child); // the input ends; the turn it asked for still finishes
/// attached.ended().await;
///
/// let mut out = Vec::new();
/// from_child.read_to_end(&mut out).await?;
/// let lines = LineDecoder::new().push(&out)?;
/// assert!(lines[0].contains(r#""type":"hello""#));
/// assert!(lines[lines.len() - 1].contains(r#""type":"reply","id":"r1","ok":true"#));
/// # Ok(())
/// # }
/// ```
pub struct Stdio<R, W> {
    read: R,
    write: W,
    start: Start,
}

impl Stdio<ProcessStdin, tokio::io::Stdout> {
    /// This process's own stdin and stdout.
    pub fn process() -> Self {
        Self::new(ProcessStdin::spawn(), tokio::io::stdout())
    }
}

/// This process's stdin, read on a thread of its own.
///
/// tokio's stdin reads on the runtime's blocking pool, and a read there
/// cannot be cancelled: a process whose conversation was closed while the
/// parent kept stdin open would hang at exit until the parent wrote again.
/// A thread of its own ends with the process.
#[derive(Debug)]
pub struct ProcessStdin {
    chunks: mpsc::Receiver<std::io::Result<Vec<u8>>>,
    chunk: Vec<u8>,
    at: usize,
}

impl ProcessStdin {
    /// Bytes read at a time.
    const CHUNK: usize = 16 * 1024;

    fn spawn() -> Self {
        let (tx, chunks) = mpsc::channel(4);
        let reader = move || {
            let mut stdin = std::io::stdin().lock();
            let mut buf = vec![0; Self::CHUNK];
            loop {
                let chunk = match stdin.read(&mut buf) {
                    Ok(0) => return,
                    Ok(n) => Ok(buf[..n].to_vec()),
                    Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(e) => Err(e),
                };
                let failed = chunk.is_err();
                if tx.blocking_send(chunk).is_err() || failed {
                    return;
                }
            }
        };
        // No thread, no input: the connection ends as if stdin had closed.
        let _ = std::thread::Builder::new()
            .name("haest-stdin".into())
            .spawn(reader);
        Self {
            chunks,
            chunk: Vec::new(),
            at: 0,
        }
    }
}

impl AsyncRead for ProcessStdin {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        out: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        loop {
            if self.at < self.chunk.len() {
                let n = out.remaining().min(self.chunk.len() - self.at);
                out.put_slice(&self.chunk[self.at..self.at + n]);
                self.at += n;
                return Poll::Ready(Ok(()));
            }
            match ready!(self.chunks.poll_recv(cx)) {
                Some(Ok(chunk)) => {
                    self.chunk = chunk;
                    self.at = 0;
                }
                Some(Err(e)) => return Poll::Ready(Err(e)),
                // Read to its end.
                None => return Poll::Ready(Ok(())),
            }
        }
    }
}

impl<R, W> Stdio<R, W> {
    /// Requests from `read`, frames to `write`; the stream followed from its
    /// first frame.
    pub fn new(read: R, write: W) -> Self {
        Self {
            read,
            write,
            start: Start {
                follow_from: Some(0),
                token: None,
            },
        }
    }

    /// Start otherwise: follow from another seq, or not until asked.
    #[must_use]
    pub fn with_start(mut self, start: Start) -> Self {
        self.start = start;
        self
    }

    /// How the connection starts.
    pub fn start(&self) -> &Start {
        &self.start
    }
}

/// How it starts; never the pipes.
impl<R, W> std::fmt::Debug for Stdio<R, W> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Stdio")
            .field("start", &self.start)
            .finish_non_exhaustive()
    }
}

impl<R, W> Transport for Stdio<R, W>
where
    R: AsyncRead + Unpin + Send + 'static,
    W: AsyncWrite + Unpin + Send + 'static,
{
    fn attach(
        self,
        backend: impl Into<BackendRef>,
        grant: Grant,
    ) -> Result<Attached, TransportError> {
        let source = NdjsonSource::new(self.read);
        let sink = NdjsonSink::new(self.write);
        let backend: BackendRef = backend.into();
        let task = tokio::spawn(serve(backend, grant, self.start, source, sink));
        Ok(Attached::new(task))
    }
}
