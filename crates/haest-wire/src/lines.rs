//! NDJSON over a byte stream: [`LineDecoder`] turns bytes that arrive in
//! pieces into whole lines, [`encode_line`] writes one.

use serde::Serialize;

/// Bytes in, whole lines out: a pipe or a socket delivers a line in pieces,
/// or several in one read.
///
/// Keeps the unfinished tail until its newline comes, so a character split
/// between two reads is put back together. A line ends at `\n`; a `\r` before
/// it is dropped and an empty line is skipped.
///
/// The limit ([`with_limit`](Self::with_limit), 64 MiB by default) bounds
/// the unfinished tail kept between pushes: a peer that never sends a newline
/// cannot fill the memory. A line that ends inside one push is not measured,
/// so a line may run past the limit by up to the size of the chunk that ends
/// it; the reader decides that size (16 KiB for haest-transport's).
///
/// ```
/// use haest_wire::LineDecoder;
///
/// let mut lines = LineDecoder::new();
/// assert_eq!(lines.push(b"{\"a\":1}\n{\"b\":")?, [r#"{"a":1}"#]);
/// assert_eq!(lines.push(b"2}\n")?, [r#"{"b":2}"#]);
/// assert_eq!(lines.flush(), None);
/// # Ok::<(), haest_wire::LineTooLong>(())
/// ```
#[derive(Debug)]
pub struct LineDecoder {
    buf: Vec<u8>,
    limit: usize,
    /// How much of `buf` has been searched for a newline (none found): the
    /// next push searches from here, so each byte is looked at once.
    scanned: usize,
    /// Bytes the newline search has looked at, ever: a test pins it linear.
    #[cfg(test)]
    examined: usize,
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
    /// The limit [`new`](Self::new) sets: 64 MiB.
    pub const DEFAULT_LIMIT: usize = 64 << 20;

    /// A decoder with nothing buffered and the default limit
    /// ([`DEFAULT_LIMIT`](Self::DEFAULT_LIMIT)).
    pub fn new() -> Self {
        Self {
            buf: Vec::new(),
            limit: Self::DEFAULT_LIMIT,
            scanned: 0,
            #[cfg(test)]
            examined: 0,
        }
    }

    /// The decoder with an unfinished line of at most `limit` bytes.
    #[must_use]
    pub fn with_limit(mut self, limit: usize) -> Self {
        self.limit = limit;
        self
    }

    /// Feed a chunk; every line it completed, in order. Bytes that are not
    /// UTF-8 become U+FFFD.
    ///
    /// # Errors
    ///
    /// [`LineTooLong`] when the unfinished line kept after this chunk passes
    /// the limit; what was buffered is dropped.
    pub fn push(&mut self, chunk: &[u8]) -> Result<Vec<String>, LineTooLong> {
        self.buf.extend_from_slice(chunk);
        let mut lines = Vec::new();
        let mut start = 0;
        // The tail kept from the last push has no newline: search only what is new.
        let mut from = self.scanned;
        while let Some(at) = self.buf[from..].iter().position(|b| *b == b'\n') {
            #[cfg(test)]
            {
                self.examined += at + 1;
            }
            let end = from + at;
            let mut line = &self.buf[start..end];
            if line.last() == Some(&b'\r') {
                line = &line[..line.len() - 1];
            }
            if !line.is_empty() {
                lines.push(String::from_utf8_lossy(line).into_owned());
            }
            start = end + 1;
            from = start;
        }
        #[cfg(test)]
        {
            self.examined += self.buf.len() - from;
        }
        self.buf.drain(..start);
        self.scanned = self.buf.len();
        if self.buf.len() > self.limit {
            self.buf.clear();
            self.scanned = 0;
            return Err(LineTooLong { limit: self.limit });
        }
        Ok(lines)
    }

    /// The last line, when the stream ended without its newline; None once
    /// it was given, or when the stream ended cleanly.
    pub fn flush(&mut self) -> Option<String> {
        let tail = String::from_utf8_lossy(&self.buf).trim().to_string();
        self.buf.clear();
        self.scanned = 0;
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
        let mut d = LineDecoder::new().with_limit(8);
        assert_eq!(d.push(b"0123456789"), Err(LineTooLong { limit: 8 }));
        assert_eq!(d.push(b"ok\n").unwrap(), ["ok"]);
    }

    /// The limit bounds the tail kept between pushes, as the docs say: a
    /// line that ends inside the push that brings it is not measured.
    #[test]
    fn the_limit_bounds_the_unfinished_tail() {
        let mut d = LineDecoder::new().with_limit(8);
        assert!(d.push(b"0123456").unwrap().is_empty());
        assert_eq!(d.push(b"789abc\nde").unwrap(), ["0123456789abc"]);
        assert_eq!(d.push(b"\n").unwrap(), ["de"]);
    }

    /// A line that arrives a byte at a time is searched once, not once per
    /// byte: a peer that never sends a newline costs linear time, not
    /// quadratic.
    #[test]
    fn every_byte_is_searched_once_however_the_lines_are_split() {
        let mut d = LineDecoder::new();
        let long = "x".repeat(10_000);
        for b in long.as_bytes() {
            assert!(d.push(std::slice::from_ref(b)).unwrap().is_empty());
        }
        // The newline split from its \r, then a whole line and a tail.
        let (cr, more, rest) = (b"\r", b"\n{\"b\":2}\n{\"c\"", b":3}\n");
        assert!(d.push(cr).unwrap().is_empty());
        assert_eq!(d.push(more).unwrap(), [&*long, "{\"b\":2}"]);
        assert_eq!(d.push(rest).unwrap(), ["{\"c\":3}"]);
        let pushed = long.len() + cr.len() + more.len() + rest.len();
        assert_eq!(d.examined, pushed);

        // A flushed tail and a refused line leave nothing behind: the next
        // line is searched from its own start.
        assert!(d.push(b"tail").unwrap().is_empty());
        assert_eq!(d.flush().as_deref(), Some("tail"));
        assert_eq!(d.push(b"ab\n").unwrap(), ["ab"]);
        let mut d = LineDecoder::new().with_limit(8);
        assert!(d.push(b"01234").unwrap().is_empty());
        assert_eq!(d.push(b"56789"), Err(LineTooLong { limit: 8 }));
        assert_eq!(d.push(b"ok\n").unwrap(), ["ok"]);
    }

    #[test]
    fn an_encoded_line_is_one_line_of_json() {
        let line = encode_line(&serde_json::json!({"a": "x\ny"}));
        assert_eq!(line, "{\"a\":\"x\\ny\"}\n");
    }
}
