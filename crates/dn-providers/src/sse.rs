//! Minimal Server-Sent Events decoder (WHATWG `text/event-stream`), enough for the
//! OpenAI, Anthropic and Gemini streaming APIs.

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SseEvent {
    /// `event:` field; empty means the default `message` type.
    pub event: String,
    pub data: String,
}

/// Incremental decoder: feed raw bytes as they arrive, get complete events out.
#[derive(Debug, Default)]
pub struct SseDecoder {
    buf: Vec<u8>,
    event: String,
    data: Vec<String>,
}

impl SseDecoder {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn feed(&mut self, bytes: &[u8]) -> Vec<SseEvent> {
        self.buf.extend_from_slice(bytes);
        let mut out = Vec::new();
        while let Some(pos) = self.buf.iter().position(|&b| b == b'\n') {
            let mut line: Vec<u8> = self.buf.drain(..=pos).collect();
            line.pop();
            if line.last() == Some(&b'\r') {
                line.pop();
            }
            self.line(&String::from_utf8_lossy(&line), &mut out);
        }
        out
    }

    /// Flush a final event that wasn't followed by a blank line.
    pub fn finish(&mut self) -> Vec<SseEvent> {
        let mut out = Vec::new();
        if !self.buf.is_empty() {
            let rest = String::from_utf8_lossy(&std::mem::take(&mut self.buf)).into_owned();
            self.line(rest.trim_end_matches('\r'), &mut out);
        }
        self.dispatch(&mut out);
        out
    }

    fn line(&mut self, line: &str, out: &mut Vec<SseEvent>) {
        if line.is_empty() {
            self.dispatch(out);
            return;
        }
        if line.starts_with(':') {
            return; // comment / keep-alive
        }
        let (field, value) = match line.split_once(':') {
            Some((f, v)) => (f, v.strip_prefix(' ').unwrap_or(v)),
            None => (line, ""),
        };
        match field {
            "event" => self.event = value.to_string(),
            "data" => self.data.push(value.to_string()),
            _ => {} // id, retry: not needed
        }
    }

    fn dispatch(&mut self, out: &mut Vec<SseEvent>) {
        if self.data.is_empty() {
            self.event.clear();
            return;
        }
        out.push(SseEvent {
            event: std::mem::take(&mut self.event),
            data: std::mem::take(&mut self.data).join("\n"),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(event: &str, data: &str) -> SseEvent {
        SseEvent {
            event: event.into(),
            data: data.into(),
        }
    }

    fn all(chunks: &[&[u8]]) -> Vec<SseEvent> {
        let mut d = SseDecoder::new();
        let mut out = Vec::new();
        for c in chunks {
            out.extend(d.feed(c));
        }
        out.extend(d.finish());
        out
    }

    #[test]
    fn single_event_with_type() {
        assert_eq!(
            all(&[b"event: message_start\ndata: {\"a\":1}\n\n"]),
            vec![ev("message_start", "{\"a\":1}")]
        );
    }

    #[test]
    fn multi_line_data_is_joined_with_newlines() {
        assert_eq!(
            all(&[b"data: first\ndata: second\ndata:\ndata: fourth\n\n"]),
            vec![ev("", "first\nsecond\n\nfourth")]
        );
    }

    #[test]
    fn comments_and_keep_alives_are_ignored() {
        assert_eq!(
            all(&[b": keep-alive\n\n:another\ndata: x\n: inline comment\n\n"]),
            vec![ev("", "x")]
        );
    }

    #[test]
    fn crlf_and_lf_line_endings() {
        assert_eq!(
            all(&[b"event: a\r\ndata: 1\r\n\r\nevent: b\ndata: 2\n\n"]),
            vec![ev("a", "1"), ev("b", "2")]
        );
    }

    #[test]
    fn crlf_split_between_chunks() {
        assert_eq!(all(&[b"data: 1\r", b"\n\r", b"\n"]), vec![ev("", "1")]);
    }

    #[test]
    fn events_split_across_chunks_at_every_byte() {
        let body = b"event: delta\ndata: {\"text\":\"h\xc3\xa9llo\"}\n\ndata: [DONE]\n\n";
        let want = vec![ev("delta", "{\"text\":\"h\u{e9}llo\"}"), ev("", "[DONE]")];
        // Every split point, including inside the two-byte UTF-8 `é`.
        for cut in 0..body.len() {
            assert_eq!(all(&[&body[..cut], &body[cut..]]), want, "cut at {cut}");
        }
        // One byte at a time.
        let bytes: Vec<&[u8]> = body.chunks(1).collect();
        assert_eq!(all(&bytes), want);
    }

    #[test]
    fn field_value_space_rules() {
        // Only one leading space is stripped; no colon means empty value.
        assert_eq!(
            all(&[b"data:  two\n\ndata\n\n"]),
            vec![ev("", " two"), ev("", "")]
        );
    }

    #[test]
    fn unknown_fields_and_empty_events_are_dropped() {
        assert_eq!(
            all(&[b"id: 7\nretry: 100\n\nevent: ping\n\ndata: ok\n\n"]),
            vec![ev("", "ok")]
        );
    }

    #[test]
    fn final_event_without_trailing_blank_line_is_flushed() {
        assert_eq!(
            all(&[b"data: a\n\ndata: tail"]),
            vec![ev("", "a"), ev("", "tail")]
        );
    }
}
