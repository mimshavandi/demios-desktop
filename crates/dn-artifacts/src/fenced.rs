//! Streaming parser for the fenced fallback used by models **without tool calling**
//! (ADR-0004). Only run model text through it when the model has no tool support; tool-capable
//! models use [`crate::protocol::tool_specs`].
//!
//! ```text
//! Here is the page:
//! <artifact id="todo" type="html" title="Todo list">
//! <!doctype html>…
//! </artifact>
//! ```
//!
//! Rules, so that prose that merely mentions or echoes a tag creates nothing:
//! - an opening tag counts only at the **start of a line** (up to 3 spaces of indent);
//! - tags inside a ```` ``` ```` or `~~~` code fence are text;
//! - the content runs to the first `</artifact>`, so content cannot contain that text;
//! - content over [`MAX_CONTENT_BYTES`] is discarded and reported as [`FenceEvent::Rejected`].
//!
//! Text outside a block passes through as [`FenceEvent::Text`]. Tags may be split across
//! chunks; the parser holds back only what could still become a tag or a fence. Claude's legacy
//! `<antArtifact identifier=… type="application/vnd.ant.react">` spelling is accepted too.

use crate::error::ArtifactError;
use crate::protocol::parse_artifact_type;
use crate::store::MAX_CONTENT_BYTES;
use dn_core::artifact::ArtifactType;

/// Opening tags longer than this are treated as plain text (guards against unbounded buffering).
const MAX_TAG_LEN: usize = 2048;

const TAG_NAMES: [&str; 2] = ["artifact", "antArtifact"];

/// Attributes of an opening tag.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FenceHeader {
    /// The model's identifier (`id=` or `identifier=`). Reusing it later rewrites the artifact.
    pub id: Option<String>,
    pub artifact_type: ArtifactType,
    /// `title=`, or `"Untitled"`.
    pub title: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FencedArtifact {
    pub header: FenceHeader,
    pub content: String,
    /// `false` for [`FenceEvent::Unterminated`]; [`crate::apply_fenced`] refuses those.
    pub complete: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FenceEvent {
    /// Prose outside any block.
    Text(String),
    /// A block started; content follows as [`FenceEvent::Content`].
    Open(FenceHeader),
    /// Live content of the open block, for streaming preview (the leading newline after the tag
    /// is dropped; [`FenceEvent::Close`] carries the authoritative, trimmed content).
    Content(String),
    /// A block ended.
    Close(FencedArtifact),
    /// The stream ended inside a block (returned by [`FencedParser::finish`]). Not to be saved.
    Unterminated(FencedArtifact),
    /// A block was dropped; `reason` is written for the model (send it back as feedback).
    Rejected { header: FenceHeader, reason: String },
}

#[derive(Debug)]
pub struct FencedParser {
    pending: String,
    open: Option<Open>,
    at_line_start: bool,
    /// Marker of the code fence we are inside (```` ``` ```` or `~~~`), if any.
    fence: Option<&'static str>,
}

impl Default for FencedParser {
    fn default() -> Self {
        Self {
            pending: String::new(),
            open: None,
            at_line_start: true,
            fence: None,
        }
    }
}

#[derive(Debug)]
struct Open {
    header: FenceHeader,
    close_tag: String,
    content: String,
    at_start: bool,
    /// Bytes seen so far; past the cap the content is dropped and only the close tag is sought.
    total: usize,
    overflow: bool,
}

impl FencedParser {
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed the next chunk of model text.
    pub fn push(&mut self, chunk: &str) -> Vec<FenceEvent> {
        self.pending.push_str(chunk);
        let mut out = Vec::new();
        loop {
            let progressed = if self.open.is_some() {
                self.step_content(&mut out)
            } else {
                self.step_text(&mut out)
            };
            if !progressed {
                break;
            }
        }
        out
    }

    /// End of stream: flush held-back text, report an unterminated block.
    pub fn finish(mut self) -> Vec<FenceEvent> {
        let mut out = Vec::new();
        match self.open.take() {
            Some(mut open) => {
                let rest = std::mem::take(&mut self.pending);
                push_content(&mut open, &rest, &mut out);
                out.push(close_event(open, false));
            }
            None if !self.pending.is_empty() => {
                out.push(FenceEvent::Text(std::mem::take(&mut self.pending)))
            }
            None => {}
        }
        out
    }

    /// Outside a block. Returns whether it consumed anything and should run again.
    fn step_text(&mut self, out: &mut Vec<FenceEvent>) -> bool {
        if self.pending.is_empty() {
            return false;
        }
        if !self.at_line_start {
            // Mid-line: nothing here can start a tag or a fence; emit through the newline.
            return match self.pending.find('\n') {
                Some(nl) => {
                    let text: String = self.pending.drain(..=nl).collect();
                    emit_text(out, text);
                    self.at_line_start = true;
                    true
                }
                None => {
                    emit_text(out, std::mem::take(&mut self.pending));
                    false
                }
            };
        }
        let indent = self.pending.bytes().take_while(|&b| b == b' ').count();
        let rest = &self.pending[indent..];
        if rest.is_empty() {
            return false; // only spaces so far
        }
        if indent > 3 {
            self.at_line_start = false; // indented code, not a tag
            return true;
        }
        for marker in ["```", "~~~"] {
            if rest.len() < marker.len() && marker.starts_with(rest) {
                return false; // could still become a fence
            }
            if rest.starts_with(marker) {
                self.fence = match self.fence {
                    Some(open) if open == marker => None,
                    Some(open) => Some(open),
                    None => Some(marker),
                };
                self.at_line_start = false;
                return true;
            }
        }
        if self.fence.is_some() || !rest.starts_with('<') {
            self.at_line_start = false;
            return true;
        }
        match match_open_tag(rest) {
            TagMatch::NeedMore => false,
            TagMatch::No => {
                self.at_line_start = false;
                true
            }
            TagMatch::Open { len, name, attrs } => {
                let lead: String = self.pending.drain(..indent).collect();
                emit_text(out, lead);
                self.pending.drain(..len);
                let header = header_from_attrs(&attrs);
                out.push(FenceEvent::Open(header.clone()));
                self.open = Some(Open {
                    header,
                    close_tag: format!("</{name}>"),
                    content: String::new(),
                    at_start: true,
                    total: 0,
                    overflow: false,
                });
                true
            }
        }
    }

    /// Inside a block.
    fn step_content(&mut self, out: &mut Vec<FenceEvent>) -> bool {
        let open = self.open.as_mut().expect("open block");
        if let Some(at) = self.pending.find(&open.close_tag) {
            let body: String = self.pending.drain(..at).collect();
            self.pending.drain(..open.close_tag.len());
            push_content(open, &body, out);
            let open = self.open.take().expect("open block");
            out.push(close_event(open, true));
            self.at_line_start = false;
            return true;
        }
        // Hold back a suffix that could be the start of the close tag.
        let keep = longest_suffix_prefix(&self.pending, &open.close_tag);
        let emit_len = self.pending.len() - keep;
        if emit_len > 0 {
            let body: String = self.pending.drain(..emit_len).collect();
            push_content(open, &body, out);
        }
        false
    }
}

fn close_event(open: Open, complete: bool) -> FenceEvent {
    if open.overflow {
        return FenceEvent::Rejected {
            reason: ArtifactError::TooLarge {
                what: "the artifact content",
                max: MAX_CONTENT_BYTES,
                actual: open.total,
            }
            .to_string(),
            header: open.header,
        };
    }
    let artifact = FencedArtifact {
        content: clean_content(&open.content, &open.header.artifact_type),
        header: open.header,
        complete,
    };
    if complete {
        FenceEvent::Close(artifact)
    } else {
        FenceEvent::Unterminated(artifact)
    }
}

fn emit_text(out: &mut Vec<FenceEvent>, text: String) {
    if text.is_empty() {
        return;
    }
    if let Some(FenceEvent::Text(prev)) = out.last_mut() {
        prev.push_str(&text);
    } else {
        out.push(FenceEvent::Text(text));
    }
}

fn push_content(open: &mut Open, mut body: &str, out: &mut Vec<FenceEvent>) {
    if open.at_start && !body.is_empty() {
        // Drop the newline right after the opening tag.
        if let Some(rest) = body
            .strip_prefix("\r\n")
            .or_else(|| body.strip_prefix('\n'))
        {
            body = rest;
        } else if body == "\r" {
            return; // wait for the '\n'
        }
        open.at_start = false;
    }
    if body.is_empty() {
        return;
    }
    open.total += body.len();
    if open.overflow || open.total > MAX_CONTENT_BYTES {
        // Stop buffering; the block will be rejected when it closes.
        open.overflow = true;
        open.content = String::new();
        return;
    }
    open.content.push_str(body);
    if let Some(FenceEvent::Content(prev)) = out.last_mut() {
        prev.push_str(body);
    } else {
        out.push(FenceEvent::Content(body.to_owned()));
    }
}

/// CRLF becomes LF and the trailing newline before the close tag is dropped. A single Markdown
/// code fence wrapping the whole body (a common small-model habit) is unwrapped, except for
/// Markdown and code artifacts, where a code block can be the real content.
fn clean_content(content: &str, ty: &ArtifactType) -> String {
    let content = content.replace("\r\n", "\n");
    let trimmed = content.strip_suffix('\n').unwrap_or(&content);
    let unwrap = !matches!(ty, ArtifactType::Markdown | ArtifactType::Code { .. });
    let t = trimmed.trim();
    if unwrap
        && let Some(rest) = t.strip_prefix("```")
        && let Some(body) = rest.strip_suffix("```")
        && let Some(nl) = body.find('\n')
        && !body[nl + 1..].contains("```")
    {
        let body = &body[nl + 1..];
        return body.strip_suffix('\n').unwrap_or(body).to_owned();
    }
    trimmed.to_owned()
}

/// Length of the longest suffix of `hay` that is a proper prefix of `needle`.
fn longest_suffix_prefix(hay: &str, needle: &str) -> usize {
    let max = needle.len().saturating_sub(1).min(hay.len());
    (1..=max)
        .rev()
        .find(|&k| hay.is_char_boundary(hay.len() - k) && needle.starts_with(&hay[hay.len() - k..]))
        .unwrap_or(0)
}

enum TagMatch {
    No,
    NeedMore,
    Open {
        len: usize,
        name: &'static str,
        attrs: Vec<(String, String)>,
    },
}

/// `s` starts with '<'.
fn match_open_tag(s: &str) -> TagMatch {
    let after_lt = &s[1..];
    let mut need_more = false;
    for name in TAG_NAMES {
        if after_lt.len() <= name.len() {
            if name.starts_with(after_lt) {
                need_more = true;
            }
            continue;
        }
        let Some(rest) = after_lt.strip_prefix(name) else {
            continue;
        };
        let next = rest.chars().next().expect("non-empty");
        if !(next.is_whitespace() || next == '>' || next == '/') {
            continue;
        }
        return match find_tag_end(rest) {
            Some(end) => match parse_attrs(&rest[..end]) {
                Some(attrs) => TagMatch::Open {
                    len: 1 + name.len() + end + 1,
                    name,
                    attrs,
                },
                None => TagMatch::No,
            },
            None if s.len() < MAX_TAG_LEN => TagMatch::NeedMore,
            None => TagMatch::No,
        };
    }
    if need_more {
        TagMatch::NeedMore
    } else {
        TagMatch::No
    }
}

/// Index of the '>' ending the tag, ignoring '>' inside quoted values.
fn find_tag_end(s: &str) -> Option<usize> {
    let mut quote = None;
    for (i, c) in s.char_indices() {
        match (quote, c) {
            (None, '"' | '\'') => quote = Some(c),
            (Some(q), c) if c == q => quote = None,
            (None, '>') => return Some(i),
            _ => {}
        }
    }
    None
}

/// `key="v" key='v' key=v` pairs. `None` if the tag is malformed.
fn parse_attrs(s: &str) -> Option<Vec<(String, String)>> {
    let mut attrs = Vec::new();
    let mut rest = s.trim_end_matches('/').trim_start();
    while !rest.is_empty() {
        let key_end = rest
            .find(|c: char| c == '=' || c.is_whitespace())
            .unwrap_or(rest.len());
        let key = &rest[..key_end];
        if key.is_empty() {
            return None;
        }
        rest = rest[key_end..].trim_start();
        let value = if let Some(r) = rest.strip_prefix('=') {
            let r = r.trim_start();
            match r.chars().next() {
                Some(q @ ('"' | '\'')) => {
                    let end = r[1..].find(q)?;
                    let v = &r[1..1 + end];
                    rest = &r[end + 2..];
                    v
                }
                _ => {
                    let end = r.find(char::is_whitespace).unwrap_or(r.len());
                    let v = &r[..end];
                    rest = &r[end..];
                    v
                }
            }
        } else {
            ""
        };
        attrs.push((key.to_ascii_lowercase(), unescape(value)));
        rest = rest.trim_start();
    }
    Some(attrs)
}

fn unescape(v: &str) -> String {
    v.replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&")
}

fn header_from_attrs(attrs: &[(String, String)]) -> FenceHeader {
    let get = |k: &str| {
        attrs
            .iter()
            .find(|(key, _)| key == k)
            .map(|(_, v)| v.trim())
            .filter(|v| !v.is_empty())
    };
    let raw_type = get("type").unwrap_or("markdown");
    let language = get("language");
    let artifact_type = parse_artifact_type(raw_type, language).unwrap_or(ArtifactType::Code {
        language: language.unwrap_or(raw_type).to_owned(),
    });
    FenceHeader {
        id: get("id").or_else(|| get("identifier")).map(str::to_owned),
        artifact_type,
        title: get("title").unwrap_or("Untitled").to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(chunks: &[&str]) -> Vec<FenceEvent> {
        let mut p = FencedParser::new();
        let mut out = Vec::new();
        for c in chunks {
            out.extend(p.push(c));
        }
        out.extend(p.finish());
        out
    }

    fn closed(events: &[FenceEvent]) -> Vec<&FencedArtifact> {
        events
            .iter()
            .filter_map(|e| match e {
                FenceEvent::Close(a) => Some(a),
                _ => None,
            })
            .collect()
    }

    fn text(events: &[FenceEvent]) -> String {
        events
            .iter()
            .filter_map(|e| match e {
                FenceEvent::Text(t) => Some(t.as_str()),
                _ => None,
            })
            .collect()
    }

    const DOC: &str = "Here you go:\n<artifact id=\"todo\" type=\"html\" title=\"Todo list\">\n<h1>Todo</h1>\n</artifact>\nEnjoy.";

    #[test]
    fn parses_a_whole_block() {
        let ev = run(&[DOC]);
        let a = closed(&ev);
        assert_eq!(a.len(), 1);
        assert_eq!(a[0].header.id.as_deref(), Some("todo"));
        assert_eq!(a[0].header.artifact_type, ArtifactType::Html);
        assert_eq!(a[0].header.title, "Todo list");
        assert_eq!(a[0].content, "<h1>Todo</h1>");
        assert_eq!(text(&ev), "Here you go:\n\nEnjoy.");
    }

    #[test]
    fn same_result_for_every_chunking() {
        let whole = run(&[DOC]);
        // Byte-by-byte and every two-way split.
        let bytes: Vec<String> = DOC.chars().map(String::from).collect();
        let refs: Vec<&str> = bytes.iter().map(String::as_str).collect();
        let by_char = run(&refs);
        assert_eq!(closed(&by_char), closed(&whole));
        assert_eq!(text(&by_char), text(&whole));
        for i in 0..=DOC.len() {
            let ev = run(&[&DOC[..i], &DOC[i..]]);
            assert_eq!(closed(&ev), closed(&whole), "split at {i}");
            assert_eq!(text(&ev), text(&whole), "split at {i}");
        }
    }

    #[test]
    fn streams_content_deltas() {
        let mut p = FencedParser::new();
        let mut ev = p.push("<artifact type=svg title=Logo>\n<svg>");
        assert!(matches!(ev[0], FenceEvent::Open(_)));
        assert_eq!(ev[1], FenceEvent::Content("<svg>".into()));
        ev = p.push("</svg></arti");
        assert_eq!(ev, [FenceEvent::Content("</svg>".into())]);
        ev = p.push("fact>");
        assert!(matches!(&ev[..], [FenceEvent::Close(a)] if a.content == "<svg></svg>"));
    }

    #[test]
    fn code_with_language_and_ant_spelling() {
        let ev = run(&[
            "<antArtifact identifier=\"calc\" type=\"application/vnd.ant.code\" language=\"python\" title=\"Calc\">\nprint(1)\n</antArtifact>",
        ]);
        let a = closed(&ev);
        assert_eq!(a[0].header.id.as_deref(), Some("calc"));
        assert_eq!(
            a[0].header.artifact_type,
            ArtifactType::Code {
                language: "python".into()
            }
        );
        assert_eq!(a[0].content, "print(1)");
    }

    #[test]
    fn react_and_quoted_gt_in_title() {
        let ev = run(&[
            "<artifact type='application/vnd.ant.react' title='a > b'>export default () => <p/>;</artifact>",
        ]);
        let a = closed(&ev);
        assert_eq!(a[0].header.artifact_type, ArtifactType::React);
        assert_eq!(a[0].header.title, "a > b");
        assert_eq!(a[0].header.id, None);
        assert_eq!(a[0].content, "export default () => <p/>;");
    }

    #[test]
    fn non_tags_pass_through() {
        let src = "Use <artifactory> or a < b, and <art>. <artifacts are fun";
        let ev = run(&[src]);
        assert!(closed(&ev).is_empty());
        assert_eq!(text(&ev), src);
    }

    #[test]
    fn unterminated_block_is_reported() {
        let ev = run(&["<artifact type=\"markdown\" title=\"Notes\">\n# Hi\n</arti"]);
        match ev.last().unwrap() {
            FenceEvent::Unterminated(a) => {
                assert_eq!(a.content, "# Hi\n</arti");
                assert!(!a.complete);
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn unwraps_markdown_code_fence_and_two_blocks() {
        let ev = run(&[
            "<artifact type=html title=A>\n```html\n<p>a</p>\n```\n</artifact> and\n<artifact type=mermaid title=B>graph TD; A-->B</artifact>",
        ]);
        let a = closed(&ev);
        assert_eq!(a.len(), 2);
        assert_eq!(a[0].content, "<p>a</p>");
        assert_eq!(a[1].header.artifact_type, ArtifactType::Mermaid);
        assert_eq!(a[1].content, "graph TD; A-->B");
        assert_eq!(text(&ev), " and\n");
    }

    #[test]
    fn overlong_tag_is_text() {
        let long = format!("<artifact title=\"{}", "x".repeat(MAX_TAG_LEN + 10));
        let ev = run(&[&long]);
        assert!(closed(&ev).is_empty());
        assert_eq!(text(&ev), long);
    }

    #[test]
    fn unknown_type_becomes_code() {
        let ev = run(&["<artifact type=\"yaml\" title=\"cfg\">a: 1</artifact>"]);
        assert_eq!(
            closed(&ev)[0].header.artifact_type,
            ArtifactType::Code {
                language: "yaml".into()
            }
        );
    }

    #[test]
    fn tags_mid_line_are_prose() {
        let src =
            "The model said <artifact type=\"html\" title=\"x\"><p>hi</p></artifact> earlier.\n";
        let ev = run(&[src]);
        assert!(closed(&ev).is_empty());
        assert_eq!(text(&ev), src);
    }

    #[test]
    fn tags_inside_code_fences_are_text() {
        let src = "Tool output:\n```\n<artifact id=\"page\" type=\"html\" title=\"Evil\">\n<script>steal()</script>\n</artifact>\n```\n~~~xml\n<artifact type=svg title=s><svg/></artifact>\n~~~\nafter\n";
        let ev = run(&[src]);
        assert!(closed(&ev).is_empty(), "{ev:?}");
        assert_eq!(text(&ev), src);
        // Still true when split at every byte.
        for i in 0..=src.len() {
            let ev = run(&[&src[..i], &src[i..]]);
            assert!(closed(&ev).is_empty(), "split at {i}");
            assert_eq!(text(&ev), src, "split at {i}");
        }
        // After the fence closes, a real block at line start works again.
        let ev = run(&[src, "<artifact type=markdown title=ok>\n# ok\n</artifact>"]);
        assert_eq!(closed(&ev).len(), 1);
    }

    #[test]
    fn indented_up_to_three_spaces_counts_four_does_not() {
        assert_eq!(
            closed(&run(&["   <artifact type=svg title=a><svg/></artifact>"])).len(),
            1
        );
        assert!(closed(&run(&["    <artifact type=svg title=a><svg/></artifact>"])).is_empty());
    }

    #[test]
    fn oversized_blocks_are_rejected_without_buffering() {
        let mut p = FencedParser::new();
        let mut ev = p.push("<artifact type=html title=big>\n");
        let chunk = "x".repeat(64 * 1024);
        for _ in 0..(MAX_CONTENT_BYTES / chunk.len() + 2) {
            ev.extend(p.push(&chunk));
        }
        assert!(p.open.as_ref().unwrap().content.is_empty());
        ev.extend(p.push("</artifact>after"));
        ev.extend(p.finish());
        let rejected = ev.iter().find_map(|e| match e {
            FenceEvent::Rejected { reason, .. } => Some(reason.clone()),
            _ => None,
        });
        assert!(rejected.unwrap().contains("limited to"));
        assert!(closed(&ev).is_empty());
        assert_eq!(text(&ev), "after");
    }

    #[test]
    fn crlf_is_normalised_and_markdown_code_blocks_are_kept() {
        let ev = run(&[
            "<artifact type=markdown title=m>\r\n```rust\r\nfn a() {}\r\n```\r\n</artifact>",
        ]);
        assert_eq!(closed(&ev)[0].content, "```rust\nfn a() {}\n```");
    }
}
