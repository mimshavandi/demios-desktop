//! Incremental parser for the in-band tags local models emit:
//! `<think>…</think>` → reasoning, `<tool_call>{json}</tool_call>` → tool calls (Hermes /
//! Qwen convention, which [`crate::prompt`] also instructs and the grammar enforces).
//!
//! When the chat template already opened the think block (the rendered prompt ends with
//! `<think>`), start with [`TagParser::in_think`]. A `</think>` seen while in text (a
//! template that opened the block without telling us) is dropped rather than shown.

use dn_core::chat::{StreamEvent, ToolCall};

pub const THINK_OPEN: &str = "<think>";
pub const THINK_CLOSE: &str = "</think>";
pub const TOOL_OPEN: &str = "<tool_call>";
pub const TOOL_CLOSE: &str = "</tool_call>";
/// Every tag the parser understands.
pub const TAGS: [&str; 4] = [THINK_OPEN, THINK_CLOSE, TOOL_OPEN, TOOL_CLOSE];

#[derive(Debug, PartialEq)]
enum State {
    Text,
    Think,
    Tool,
}

#[derive(Debug)]
pub struct TagParser {
    state: State,
    /// Unemitted text (may end in a partial tag).
    buf: String,
    tool_buf: String,
    calls: u32,
    /// Recognise `<tool_call>` (only when the request offered tools).
    tools: bool,
}

impl Default for TagParser {
    fn default() -> Self {
        Self::new()
    }
}

impl TagParser {
    /// Parser that starts in text and recognises `<think>` and `<tool_call>`.
    pub fn new() -> Self {
        Self {
            state: State::Text,
            buf: String::new(),
            tool_buf: String::new(),
            calls: 0,
            tools: true,
        }
    }

    /// Start inside a think block (the prompt already ends with `<think>`).
    pub fn in_think(mut self, yes: bool) -> Self {
        if yes {
            self.state = State::Think;
        }
        self
    }

    /// Whether `<tool_call>` is parsed. Off: the text passes through unchanged, so a request
    /// without tools never yields `ToolCall` events.
    pub fn with_tools(mut self, yes: bool) -> Self {
        self.tools = yes;
        self
    }

    fn tags(&self) -> &'static [&'static str] {
        match (&self.state, self.tools) {
            (State::Text, true) => &[THINK_OPEN, THINK_CLOSE, TOOL_OPEN],
            (State::Text, false) => &[THINK_OPEN, THINK_CLOSE],
            (State::Think, _) => &[THINK_CLOSE],
            (State::Tool, _) => &[TOOL_CLOSE],
        }
    }

    /// Number of tool calls emitted so far.
    pub fn tool_calls(&self) -> u32 {
        self.calls
    }

    pub fn push(&mut self, chunk: &str) -> Vec<StreamEvent> {
        self.buf.push_str(chunk);
        let mut out = vec![];
        loop {
            let tags = self.tags();
            // Earliest complete tag.
            let hit = tags
                .iter()
                .filter_map(|t| self.buf.find(t).map(|i| (i, *t)))
                .min_by_key(|(i, _)| *i);
            match hit {
                Some((i, tag)) => {
                    let before: String = self.buf[..i].to_string();
                    self.buf.drain(..i + tag.len());
                    self.emit(&before, &mut out);
                    self.transition(tag, &mut out);
                }
                None => {
                    // Keep back a suffix that could be the start of a tag.
                    let keep = tags
                        .iter()
                        .map(|t| partial_suffix(&self.buf, t))
                        .max()
                        .unwrap_or(0);
                    let cut = self.buf.len() - keep;
                    let ready: String = self.buf[..cut].to_string();
                    self.buf.drain(..cut);
                    self.emit(&ready, &mut out);
                    return out;
                }
            }
        }
    }

    /// Flush at end of generation. An unterminated tool call is parsed if its JSON is complete.
    pub fn finish(&mut self) -> Vec<StreamEvent> {
        let mut out = vec![];
        let rest = std::mem::take(&mut self.buf);
        self.emit(&rest, &mut out);
        if self.state == State::Tool {
            self.close_tool(&mut out);
        }
        self.state = State::Text;
        out
    }

    fn emit(&mut self, s: &str, out: &mut Vec<StreamEvent>) {
        if s.is_empty() {
            return;
        }
        match self.state {
            State::Text => out.push(StreamEvent::TextDelta { text: s.into() }),
            State::Think => out.push(StreamEvent::ReasoningDelta { text: s.into() }),
            State::Tool => self.tool_buf.push_str(s),
        }
    }

    fn transition(&mut self, tag: &str, out: &mut Vec<StreamEvent>) {
        self.state = match tag {
            THINK_OPEN => State::Think,
            TOOL_OPEN => {
                self.tool_buf.clear();
                State::Tool
            }
            TOOL_CLOSE => {
                self.close_tool(out);
                State::Text
            }
            _ => State::Text,
        };
    }

    fn close_tool(&mut self, out: &mut Vec<StreamEvent>) {
        let raw = std::mem::take(&mut self.tool_buf);
        match parse_tool_json(raw.trim()) {
            Some((name, arguments)) => {
                self.calls += 1;
                out.push(StreamEvent::ToolCall {
                    call: ToolCall {
                        id: format!("call_{}", self.calls),
                        name,
                        arguments,
                    },
                });
            }
            // Not a valid call: surface it as text rather than dropping it.
            None => out.push(StreamEvent::TextDelta {
                text: format!("{TOOL_OPEN}{raw}"),
            }),
        }
    }
}

fn parse_tool_json(s: &str) -> Option<(String, serde_json::Value)> {
    let v: serde_json::Value = serde_json::from_str(s).ok()?;
    let name = v.get("name")?.as_str()?.to_string();
    let args = match v.get("arguments").or_else(|| v.get("parameters")) {
        Some(serde_json::Value::String(s)) => serde_json::from_str(s).ok()?,
        Some(a) => a.clone(),
        None => serde_json::json!({}),
    };
    Some((name, args))
}

/// Length of the longest suffix of `s` that is a proper prefix of `tag`.
fn partial_suffix(s: &str, tag: &str) -> usize {
    (1..tag.len().min(s.len() + 1))
        .rev()
        .find(|&n| s.is_char_boundary(s.len() - n) && tag.starts_with(&s[s.len() - n..]))
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn run(chunks: &[&str]) -> Vec<StreamEvent> {
        let mut p = TagParser::new();
        let mut out: Vec<StreamEvent> = chunks.iter().flat_map(|c| p.push(c)).collect();
        out.extend(p.finish());
        // Merge adjacent deltas so assertions don't depend on chunking.
        let mut merged: Vec<StreamEvent> = vec![];
        for e in out {
            match (merged.last_mut(), e) {
                (Some(StreamEvent::TextDelta { text: a }), StreamEvent::TextDelta { text: b }) => {
                    a.push_str(&b)
                }
                (
                    Some(StreamEvent::ReasoningDelta { text: a }),
                    StreamEvent::ReasoningDelta { text: b },
                ) => a.push_str(&b),
                (_, e) => merged.push(e),
            }
        }
        merged
    }

    #[test]
    fn plain_text_passes_through() {
        assert_eq!(
            run(&["Hel", "lo <b>"]),
            vec![StreamEvent::TextDelta {
                text: "Hello <b>".into()
            }]
        );
    }

    #[test]
    fn think_split_across_chunks() {
        assert_eq!(
            run(&["<th", "ink>plan</thi", "nk>answer"]),
            vec![
                StreamEvent::ReasoningDelta {
                    text: "plan".into()
                },
                StreamEvent::TextDelta {
                    text: "answer".into()
                }
            ]
        );
    }

    #[test]
    fn tool_call_parsed() {
        let ev = run(&[
            "Let me check.<tool",
            "_call>\n{\"name\": \"get_weather\", \"arguments\": {\"city\": \"Ber",
            "lin\"}}\n</tool_call>",
        ]);
        assert_eq!(
            ev,
            vec![
                StreamEvent::TextDelta {
                    text: "Let me check.".into()
                },
                StreamEvent::ToolCall {
                    call: ToolCall {
                        id: "call_1".into(),
                        name: "get_weather".into(),
                        arguments: json!({"city": "Berlin"})
                    }
                }
            ]
        );
    }

    #[test]
    fn unterminated_call_parsed_on_finish() {
        let ev = run(&["<tool_call>{\"name\":\"a\",\"arguments\":\"{}\"}"]);
        assert!(matches!(&ev[0], StreamEvent::ToolCall { call } if call.name == "a"));
    }

    #[test]
    fn starts_in_think_when_prompt_opened_it() {
        let mut p = TagParser::new().in_think(true);
        let mut ev = p.push("plan</think>answer");
        ev.extend(p.finish());
        assert_eq!(
            ev,
            vec![
                StreamEvent::ReasoningDelta {
                    text: "plan".into()
                },
                StreamEvent::TextDelta {
                    text: "answer".into()
                }
            ]
        );
    }

    #[test]
    fn lone_think_close_dropped() {
        assert_eq!(
            run(&["a</thi", "nk>b"]),
            vec![StreamEvent::TextDelta { text: "ab".into() }]
        );
    }

    #[test]
    fn tool_tags_are_text_without_tools() {
        let mut p = TagParser::new().with_tools(false);
        let mut ev = p.push("<tool_call>{\"name\":\"a\"}</tool_call>");
        ev.extend(p.finish());
        let text: String = ev
            .iter()
            .map(|e| match e {
                StreamEvent::TextDelta { text } => text.as_str(),
                other => panic!("unexpected {other:?}"),
            })
            .collect();
        assert_eq!(text, "<tool_call>{\"name\":\"a\"}</tool_call>");
        assert_eq!(p.tool_calls(), 0);
    }

    #[test]
    fn invalid_call_kept_as_text() {
        let ev = run(&["<tool_call>oops</tool_call>"]);
        assert_eq!(
            ev,
            vec![StreamEvent::TextDelta {
                text: "<tool_call>oops".into()
            }]
        );
    }

    #[test]
    fn multibyte_safe() {
        assert_eq!(
            run(&["héllo <", "ü"]),
            vec![StreamEvent::TextDelta {
                text: "héllo <ü".into()
            }]
        );
    }
}
