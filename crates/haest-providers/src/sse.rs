//! Server-sent events, as every model API streams them: `event:` and `data:`
//! lines, a blank line ends an event, `:` lines are comments (OpenRouter's
//! keep-alive). Bytes arrive in arbitrary chunks, so a line (and a multi-byte
//! character) may be split anywhere; the parser buffers until a line is whole.

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SseEvent {
    /// The `event:` name, when the server sent one.
    pub event: Option<String>,
    /// Every `data:` line of the event, joined with `\n`.
    pub data: String,
}

#[derive(Debug, Default)]
pub struct SseParser {
    buf: Vec<u8>,
    event: Option<String>,
    data: Vec<String>,
}

impl SseParser {
    /// Feed a chunk; returns the events it completed.
    pub fn push(&mut self, bytes: &[u8]) -> Vec<SseEvent> {
        self.buf.extend_from_slice(bytes);
        let mut out = Vec::new();
        let mut start = 0;
        let mut i = 0;
        while i < self.buf.len() {
            let b = self.buf[i];
            if b != b'\n' && b != b'\r' {
                i += 1;
                continue;
            }
            // A lone `\r` at the end might be the first half of `\r\n`: wait for more.
            if b == b'\r' && i + 1 == self.buf.len() {
                break;
            }
            let line = String::from_utf8_lossy(&self.buf[start..i]).into_owned();
            let width = if b == b'\r' && self.buf[i + 1] == b'\n' {
                2
            } else {
                1
            };
            i += width;
            start = i;
            self.line(&line, &mut out);
        }
        self.buf.drain(..start);
        out
    }

    /// The body ended: a last event without its blank line still counts.
    pub fn finish(&mut self) -> Vec<SseEvent> {
        let mut out = Vec::new();
        if !self.buf.is_empty() {
            let rest = std::mem::take(&mut self.buf);
            let line = String::from_utf8_lossy(&rest);
            self.line(line.trim_end_matches('\r'), &mut out);
        }
        self.line("", &mut out);
        out
    }

    fn line(&mut self, line: &str, out: &mut Vec<SseEvent>) {
        if line.is_empty() {
            let event = self.event.take();
            if !self.data.is_empty() {
                out.push(SseEvent {
                    event,
                    data: std::mem::take(&mut self.data).join("\n"),
                });
            }
            return;
        }
        if line.starts_with(':') {
            return;
        }
        let (field, value) = match line.split_once(':') {
            Some((f, v)) => (f, v.strip_prefix(' ').unwrap_or(v)),
            None => (line, ""),
        };
        match field {
            "event" => self.event = Some(value.to_string()),
            "data" => self.data.push(value.to_string()),
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_in_chunks(body: &[u8], size: usize) -> Vec<SseEvent> {
        let mut p = SseParser::default();
        let mut out = Vec::new();
        for chunk in body.chunks(size) {
            out.extend(p.push(chunk));
        }
        out.extend(p.finish());
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

    #[test]
    fn bare_cr_line_endings_split_lines() {
        let got = parse_in_chunks(b"data: a\r\rdata: b\r\r", 2);
        assert_eq!(
            got.iter().map(|e| e.data.as_str()).collect::<Vec<_>>(),
            ["a", "b"]
        );
    }
}
