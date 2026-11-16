//! NDJSON over a byte stream: [`LineDecoder`] turns bytes that arrive in
//! pieces into whole lines, [`encode_line`] writes one.

use serde::Serialize;

/// Bytes in, whole lines out: a pipe or a socket delivers a line in pieces,
/// or several in one read.
///
/// Keeps the unfinished tail until its newline comes, so a character split
/// between two reads is put back together. A line ends at `\n`; a `\r` before
/// it is dropped and an empty line is skipped. A line longer than the limit
/// ([`with_limit`](Self::with_limit), 64 MiB by default) is refused: a peer
/// that never sends a newline cannot fill the memory.
///
/// ```
/// use haest_wire::LineDecoder;
///
/// let mut lines = LineDecoder::new();
/// assert_eq!(lines.push(b"{\"a\":1}\n{\"b\":").unwrap(), [r#"{"a":1}"#]);
/// assert_eq!(lines.push(b"2}\n").unwrap(), [r#"{"b":2}"#]);
/// assert_eq!(lines.flush(), None);
/// ```
#[derive(Debug)]
pub struct LineDecoder {
    buf: Vec<u8>,
    limit: usize,
}

/// A line went past the [`LineDecoder`]'s limit without its newline.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("a line is longer than {limit} bytes")]
pub struct LineTooLong {
    /// The limit, in bytes.
    pub limit: usize,
}

impl Default for LineDecoder {
    fn default() -> Self {
        Self::new()
    }
}

impl LineDecoder {
    /// A decoder with nothing buffered and the default limit (64 MiB).
    pub fn new() -> Self {
        Self::with_limit(64 << 20)
    }

    /// A decoder whose lines may be at most `limit` bytes.
    pub fn with_limit(limit: usize) -> Self {
        Self {
            buf: Vec::new(),
            limit,
        }
    }

    /// Feed a chunk; every line it completed, in order. Bytes that are not
    /// UTF-8 become U+FFFD.
    ///
    /// # Errors
    ///
    /// [`LineTooLong`] when the unfinished line passes the limit; what was
    /// buffered is dropped.
    pub fn push(&mut self, chunk: &[u8]) -> Result<Vec<String>, LineTooLong> {
        self.buf.extend_from_slice(chunk);
        let mut lines = Vec::new();
        let mut start = 0;
        while let Some(at) = self.buf[start..].iter().position(|b| *b == b'\n') {
            let end = start + at;
            let mut line = &self.buf[start..end];
            if line.last() == Some(&b'\r') {
                line = &line[..line.len() - 1];
            }
            if !line.is_empty() {
                lines.push(String::from_utf8_lossy(line).into_owned());
            }
            start = end + 1;
        }
        self.buf.drain(..start);
        if self.buf.len() > self.limit {
            self.buf.clear();
            return Err(LineTooLong { limit: self.limit });
        }
        Ok(lines)
    }

    /// The last line, when the stream ended without its newline; None once
    /// it was given, or when the stream ended cleanly.
    pub fn flush(&mut self) -> Option<String> {
        let tail = String::from_utf8_lossy(&self.buf).trim().to_string();
        self.buf.clear();
        (!tail.is_empty()).then_some(tail)
    }
}

/// `value` as one NDJSON line: compact JSON and a newline.
///
/// # Panics
///
/// When `value`'s `Serialize` fails, which the wire's types never do (a map
/// with non-string keys would).
pub fn encode_line<T: Serialize + ?Sized>(value: &T) -> String {
    let mut line = serde_json::to_string(value).expect("a wire frame serializes");
    line.push('\n');
    line
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Port of ndjson.test.ts.
    #[test]
    fn whole_lines_come_out_and_the_tail_waits() {
        let mut d = LineDecoder::new();
        assert_eq!(
            d.push(b"{\"a\":1}\n{\"b\":2}\n").unwrap(),
            ["{\"a\":1}", "{\"b\":2}"]
        );
        assert!(d.push(b"{\"c\":").unwrap().is_empty());
        assert_eq!(d.push(b"3}\n").unwrap(), ["{\"c\":3}"]);
    }

    #[test]
    fn a_character_split_between_reads_is_put_back_together() {
        let mut d = LineDecoder::new();
        let full = "{\"s\":\"éé\"}\n".as_bytes();
        assert!(d.push(&full[..7]).unwrap().is_empty());
        assert_eq!(d.push(&full[7..]).unwrap(), ["{\"s\":\"éé\"}"]);
    }

    #[test]
    fn crlf_is_a_newline_and_empty_lines_are_skipped() {
        let mut d = LineDecoder::new();
        assert_eq!(
            d.push(b"{\"a\":1}\r\n\n{\"b\":2}\n").unwrap(),
            ["{\"a\":1}", "{\"b\":2}"]
        );
    }

    #[test]
    fn flush_gives_an_unfinished_last_line_once() {
        let mut d = LineDecoder::new();
        assert!(d.push(b"{\"tail\":1}").unwrap().is_empty());
        assert_eq!(d.flush().as_deref(), Some("{\"tail\":1}"));
        assert_eq!(d.flush(), None);
    }

    #[test]
    fn flush_gives_nothing_after_a_clean_end() {
        let mut d = LineDecoder::new();
        d.push(b"{\"a\":1}\n").unwrap();
        assert_eq!(d.flush(), None);
    }

    /// A peer that never sends a newline is refused, and the decoder works on.
    #[test]
    fn a_line_past_the_limit_is_refused_and_dropped() {
        let mut d = LineDecoder::with_limit(8);
        assert_eq!(d.push(b"0123456789"), Err(LineTooLong { limit: 8 }));
        assert_eq!(d.push(b"ok\n").unwrap(), ["ok"]);
    }

    #[test]
    fn an_encoded_line_is_one_line_of_json() {
        let line = encode_line(&serde_json::json!({"a": "x\ny"}));
        assert_eq!(line, "{\"a\":\"x\\ny\"}\n");
    }
}
