//! Server-sent events, as every model API streams them: `event:` and `data:`
//! lines, a blank line ends an event, `:` lines are comments (OpenRouter's
//! keep-alive). Bytes arrive in arbitrary chunks, so a line (and a multi-byte
//! character) may be split anywhere; the parser buffers until a line is whole.
//!
//! The answer is outside input, so nothing it sends is buffered without
//! bound: one line, or one event's data, past [`MAX_EVENT`] ends the stream
//! ([`TooLong`]). Each byte is searched for a line end once, however the
//! line is chunked.

use std::mem;

/// The most one line, or one event's joined data, may hold: 16 MiB. A whole
/// answer in one event (the Responses API's `response.completed`) is far
/// below it; a stream past it is broken or hostile.
pub(crate) const MAX_EVENT: usize = 16 << 20;

/// A line or an event past [`MAX_EVENT`]: the stream ends with this as its
/// failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("the answer sent a line or an event of more than {} MiB", MAX_EVENT >> 20)]
pub(crate) struct TooLong;

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct SseEvent {
    /// The `event:` name, when the server sent one.
    pub(crate) event: Option<String>,
    /// Every `data:` line of the event, joined with `\n`.
    pub(crate) data: String,
}

#[derive(Debug, Default)]
pub(crate) struct SseParser {
    /// The line not yet ended.
    buf: Vec<u8>,
    /// How much of `buf` has been searched: no line ends before it.
    searched: usize,
    event: Option<String>,
    data: Vec<String>,
    /// The bytes `data` joins into, its newlines included.
    data_len: usize,
}

impl SseParser {
    /// Feed a chunk; returns the events it completed.
    ///
    /// Err when a line or an event passes [`MAX_EVENT`]: the stream is over.
    pub(crate) fn push(&mut self, bytes: &[u8]) -> Result<Vec<SseEvent>, TooLong> {
        self.buf.extend_from_slice(bytes);
        let mut out = Vec::new();
        let mut start = 0;
        let mut from = self.searched;
        let mut searched = self.buf.len();
        while let Some(at) = self.buf[from..]
            .iter()
            .position(|&b| b == b'\n' || b == b'\r')
        {
            let end = from + at;
            let cr = self.buf[end] == b'\r';
            // A lone `\r` at the end might be the first half of `\r\n`: wait for more.
            if cr && end + 1 == self.buf.len() {
                searched = end;
                break;
            }
            let width = if cr && self.buf[end + 1] == b'\n' {
                2
            } else {
                1
            };
            let line = String::from_utf8_lossy(&self.buf[start..end]).into_owned();
            self.line(&line, &mut out)?;
            from = end + width;
            start = from;
        }
        self.buf.drain(..start);
        self.searched = searched - start;
        if self.buf.len() > MAX_EVENT {
            return Err(TooLong);
        }
        Ok(out)
    }

    /// The body ended: a last event without its blank line still counts.
    ///
    /// Err when that last event passes [`MAX_EVENT`].
    pub(crate) fn finish(&mut self) -> Result<Vec<SseEvent>, TooLong> {
        let mut out = Vec::new();
        if !self.buf.is_empty() {
            let rest = mem::take(&mut self.buf);
            self.searched = 0;
            let line = String::from_utf8_lossy(&rest);
            self.line(line.trim_end_matches('\r'), &mut out)?;
        }
        self.line("", &mut out)?;
        Ok(out)
    }

    fn line(&mut self, line: &str, out: &mut Vec<SseEvent>) -> Result<(), TooLong> {
        if line.is_empty() {
            let event = self.event.take();
            if !self.data.is_empty() {
                self.data_len = 0;
                out.push(SseEvent {
                    event,
                    data: mem::take(&mut self.data).join("\n"),
                });
            }
            return Ok(());
        }
        if line.starts_with(':') {
            return Ok(());
        }
        let (field, value) = match line.split_once(':') {
            Some((f, v)) => (f, v.strip_prefix(' ').unwrap_or(v)),
            None => (line, ""),
        };
        match field {
            "event" => self.event = Some(value.to_string()),
            "data" => {
                // Joined with `\n`: every line after the first adds one.
                self.data_len += value.len() + usize::from(!self.data.is_empty());
                if self.data_len > MAX_EVENT {
                    return Err(TooLong);
                }
                self.data.push(value.to_string());
            }
            _ => {}
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_in_chunks(body: &[u8], size: usize) -> Vec<SseEvent> {
        let mut p = SseParser::default();
        let mut out = Vec::new();
        for chunk in body.chunks(size) {
            out.extend(p.push(chunk).expect("under the cap"));
        }
        out.extend(p.finish().expect("under the cap"));
        out
    }

    #[test]
    fn events_survive_any_chunking_including_split_crlf_and_utf8() {
        let body = "event: a\r\ndata: {\"t\":\"h\u{e9}\u{1f600}\"}\r\n\r\n: keep-alive\n\ndata: x\ndata: y\n\n";
        let want = vec![
            SseEvent {
                event: Some("a".into()),
                data: "{\"t\":\"h\u{e9}\u{1f600}\"}".into(),
            },
            SseEvent {
                event: None,
                data: "x\ny".into(),
            },
        ];
        for size in 1..body.len() {
            assert_eq!(
                parse_in_chunks(body.as_bytes(), size),
                want,
                "chunk size {size}"
            );
        }
    }

    #[test]
    fn a_trailing_event_without_its_blank_line_still_arrives() {
        assert_eq!(
            parse_in_chunks(b"data: [DONE]", 4),
            vec![SseEvent {
                event: None,
                data: "[DONE]".into()
            }]
        );
    }

    #[test]
    fn an_event_with_no_data_is_not_dispatched_and_does_not_leak_its_name() {
        let got = parse_in_chunks(b"event: ping\n\ndata: 1\n\n", 3);
        assert_eq!(
            got,
            vec![SseEvent {
                event: None,
                data: "1".into()
            }]
        );
    }

    /// Each byte is looked at once: a line arriving in many small chunks
    /// costs its length, not its length times the chunks (2 MiB in 1 KiB
    /// chunks took seconds when every chunk rescanned the line).
    #[test]
    fn a_long_line_in_small_chunks_is_scanned_once() {
        let mut p = SseParser::default();
        let chunk = [b'x'; 1024];
        let started = std::time::Instant::now();
        let mut events = p.push(b"data: ").unwrap();
        for _ in 0..2048 {
            events.extend(p.push(&chunk).unwrap());
        }
        events.extend(p.push(b"\n\n").unwrap());
        let took = started.elapsed();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].data.len(), 2 << 20);
        assert!(took < std::time::Duration::from_secs(1), "took {took:?}");
    }

    /// The cap holds for a line still arriving, and for an event made of
    /// many lines each under it; up to it, everything arrives.
    #[test]
    fn a_line_or_an_event_past_the_cap_is_refused() {
        let mut p = SseParser::default();
        p.push(b"data: ").unwrap();
        assert_eq!(p.push(&vec![b'x'; MAX_EVENT]), Err(TooLong));

        let half = format!("data: {}\n", "x".repeat(MAX_EVENT / 2));
        let mut p = SseParser::default();
        p.push(half.as_bytes()).unwrap();
        assert_eq!(p.push(half.as_bytes()), Err(TooLong), "the newline tips it");

        let mut p = SseParser::default();
        let mut whole = format!("data: {}", "x".repeat(MAX_EVENT - 1)).into_bytes();
        whole.extend_from_slice(b"\n\n");
        let events = p.push(&whole).unwrap();
        assert_eq!(events[0].data.len(), MAX_EVENT - 1);
    }

    /// A `\r` that ends a chunk may be half a `\r\n`; the search resumes there.
    #[test]
    fn a_cr_at_a_chunk_end_is_searched_again_with_the_next_chunk() {
        let mut p = SseParser::default();
        assert!(p.push(b"data: a\r").unwrap().is_empty());
        let events = p.push(b"\n\ndata: b\r").unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(p.push(b"\r\n").unwrap()[0].data, "b");
    }

    #[test]
    fn bare_cr_line_endings_split_lines() {
        let got = parse_in_chunks(b"data: a\r\rdata: b\r\r", 2);
        assert_eq!(
            got.iter().map(|e| e.data.as_str()).collect::<Vec<_>>(),
            ["a", "b"]
        );
    }
}
