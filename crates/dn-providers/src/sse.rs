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

    #[test]
    fn splits_across_chunks_and_handles_crlf() {
        let mut d = SseDecoder::new();
        assert!(d.feed(b"event: ping\r\ndata: {\"a\"").is_empty());
        let ev = d.feed(b":1}\r\n\r\n: keep-alive\n\ndata: x\ndata: y\n\n");
        assert_eq!(
            ev,
            vec![
                SseEvent {
                    event: "ping".into(),
                    data: "{\"a\":1}".into()
                },
                SseEvent {
                    event: "".into(),
                    data: "x\ny".into()
                }
            ]
        );
        assert!(d.feed(b"data: tail").is_empty());
        assert_eq!(d.finish()[0].data, "tail");
    }
}
