//! Lines over a byte stream: one JSON object per line, `\n` after each
//! (NDJSON). What stdio and the control socket carry.

use std::collections::VecDeque;

use haest_wire::LineDecoder;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use crate::{LineSink, LineSource, TransportError};

/// Bytes read at a time.
const READ_CHUNK: usize = 16 * 1024;

/// Requests from a byte stream, a line each, however the bytes arrive.
pub struct NdjsonSource<R> {
    read: R,
    lines: LineDecoder,
    ready: VecDeque<String>,
    /// What each read fills: made on the first read, then kept.
    chunk: Vec<u8>,
    ended: bool,
}

impl<R> NdjsonSource<R> {
    /// Lines from `read`, each at most [`LineDecoder::DEFAULT_LIMIT`] bytes.
    pub fn new(read: R) -> Self {
        Self {
            read,
            lines: LineDecoder::new(),
            ready: VecDeque::new(),
            chunk: Vec::new(),
            ended: false,
        }
    }

    /// Lines of at most `limit` bytes: a longer one ends the stream with
    /// [`TransportError::LineTooLong`].
    #[must_use]
    pub fn with_limit(mut self, limit: usize) -> Self {
        self.lines = std::mem::take(&mut self.lines).with_limit(limit);
        self
    }

    /// The most a line may hold, in bytes ([`with_limit`](Self::with_limit)).
    pub fn limit(&self) -> usize {
        self.lines.limit()
    }
}

/// How many lines wait and whether the stream ended; never their text.
impl<R> std::fmt::Debug for NdjsonSource<R> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NdjsonSource")
            .field("ready", &self.ready.len())
            .field("ended", &self.ended)
            .finish_non_exhaustive()
    }
}

impl<R: AsyncRead + Unpin + Send> LineSource for NdjsonSource<R> {
    /// Cancel-safe: a line read but not yet taken stays for the next call.
    async fn next_line(&mut self) -> Option<Result<String, TransportError>> {
        loop {
            if let Some(line) = self.ready.pop_front() {
                return Some(Ok(line));
            }
            if self.ended {
                return None;
            }
            if self.chunk.is_empty() {
                self.chunk = vec![0; READ_CHUNK];
            }
            let n = match self.read.read(&mut self.chunk).await {
                Ok(n) => n,
                Err(e) => return Some(Err(e.into())),
            };
            if n == 0 {
                self.ended = true;
                self.ready.extend(self.lines.flush());
                continue;
            }
            match self.lines.push(&self.chunk[..n]) {
                Ok(lines) => self.ready.extend(lines),
                Err(e) => return Some(Err(e.into())),
            }
        }
    }
}

/// Frames to a byte stream, a line each, flushed as they go.
pub struct NdjsonSink<W> {
    write: W,
}

impl<W> NdjsonSink<W> {
    /// Lines to `write`.
    pub fn new(write: W) -> Self {
        Self { write }
    }
}

/// Holds nothing to show but its writer.
impl<W> std::fmt::Debug for NdjsonSink<W> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NdjsonSink").finish_non_exhaustive()
    }
}

impl<W: AsyncWrite + Unpin + Send + 'static> LineSink for NdjsonSink<W> {
    async fn send_line(&mut self, mut line: String) -> Result<(), TransportError> {
        line.push('\n');
        self.write.write_all(line.as_bytes()).await?;
        self.write.flush().await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use tokio::io::AsyncWriteExt;

    use super::*;

    #[tokio::test]
    async fn lines_come_whole_however_the_bytes_arrive_and_a_last_one_without_its_newline_too() {
        let (mut peer, ours) = tokio::io::duplex(64);
        let mut source = NdjsonSource::new(ours);
        let writing = tokio::spawn(async move {
            for piece in [&b"{\"a\":"[..], b"1}\n\n{\"b\":2}\r\n{\"c\"", b":3}"] {
                peer.write_all(piece).await.unwrap();
                tokio::task::yield_now().await;
            }
        });
        let mut lines = Vec::new();
        while let Some(line) = source.next_line().await {
            lines.push(line.unwrap());
        }
        writing.await.unwrap();
        assert_eq!(lines, [r#"{"a":1}"#, r#"{"b":2}"#, r#"{"c":3}"#]);
    }

    #[tokio::test]
    async fn a_line_past_the_hosts_limit_is_refused() {
        let (mut peer, ours) = tokio::io::duplex(1 << 16);
        assert_eq!(
            NdjsonSource::new(&b""[..]).limit(),
            LineDecoder::DEFAULT_LIMIT
        );
        let mut source = NdjsonSource::new(ours).with_limit(8);
        assert_eq!(source.limit(), 8);
        peer.write_all(b"0123456789abcdef").await.unwrap();
        let err = source.next_line().await.unwrap().unwrap_err();
        assert!(matches!(err, TransportError::LineTooLong(_)), "{err}");
    }

    /// A line already waiting is handed over without a read, and every read
    /// fills the one buffer the source keeps.
    #[tokio::test]
    async fn a_waiting_line_needs_no_read_and_reads_reuse_one_buffer() {
        let mut source = NdjsonSource::new(&b"{\"a\":1}\n{\"b\":2}\n"[..]);
        assert!(source.chunk.is_empty(), "nothing is made before a read");
        assert_eq!(source.next_line().await.unwrap().unwrap(), r#"{"a":1}"#);
        let made = source.chunk.as_ptr();
        assert_eq!(source.next_line().await.unwrap().unwrap(), r#"{"b":2}"#);
        assert!(source.next_line().await.is_none());
        assert_eq!(source.chunk.as_ptr(), made, "the same buffer");
    }

    #[tokio::test]
    async fn each_frame_is_one_line_written_out_at_once() {
        let (ours, mut peer) = tokio::io::duplex(1 << 16);
        let mut sink = NdjsonSink::new(ours);
        sink.send_line(r#"{"a":1}"#.into()).await.unwrap();
        sink.send_line(r#"{"b":2}"#.into()).await.unwrap();
        drop(sink);
        let mut out = String::new();
        tokio::io::AsyncReadExt::read_to_string(&mut peer, &mut out)
            .await
            .unwrap();
        assert_eq!(out, "{\"a\":1}\n{\"b\":2}\n");
    }
}
