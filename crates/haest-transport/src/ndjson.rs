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
    ended: bool,
}

impl<R> NdjsonSource<R> {
    /// Lines from `read`.
    pub fn new(read: R) -> Self {
        Self {
            read,
            lines: LineDecoder::new(),
            ready: VecDeque::new(),
            ended: false,
        }
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
        let mut chunk = vec![0; READ_CHUNK];
        loop {
            if let Some(line) = self.ready.pop_front() {
                return Some(Ok(line));
            }
            if self.ended {
                return None;
            }
            let n = match self.read.read(&mut chunk).await {
                Ok(n) => n,
                Err(e) => return Some(Err(e.into())),
            };
            if n == 0 {
                self.ended = true;
                self.ready.extend(self.lines.flush());
                continue;
            }
            match self.lines.push(&chunk[..n]) {
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
    async fn a_line_that_never_ends_is_refused() {
        let (mut peer, ours) = tokio::io::duplex(1 << 16);
        let mut source = NdjsonSource {
            lines: LineDecoder::new().with_limit(8),
            ..NdjsonSource::new(ours)
        };
        peer.write_all(b"0123456789abcdef").await.unwrap();
        let err = source.next_line().await.unwrap().unwrap_err();
        assert!(matches!(err, TransportError::LineTooLong(_)), "{err}");
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
