//! Splitting a text file into line-addressed chunks.

/// Lines per chunk at most.
pub const CHUNK_LINES: usize = 40;
/// Characters per chunk at most (a chunk ends early at a line boundary when it would grow past).
pub const CHUNK_CHARS: usize = 2_000;
/// Lines repeated at the start of the next chunk, so a passage split across two chunks is found.
pub const OVERLAP_LINES: usize = 4;
/// Files larger than this are not indexed.
pub const MAX_FILE_BYTES: usize = 2 * 1024 * 1024;

/// One chunk of a file: its text and its 1-based, inclusive line range.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Chunk {
    pub start_line: u32,
    pub end_line: u32,
    pub text: String,
}

/// Why a file was not indexed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Skip {
    /// Over [`MAX_FILE_BYTES`].
    TooLarge,
    /// Not UTF-8 text, or contains NUL bytes.
    NotText,
    /// No text in it.
    Empty,
}

/// The file's text, if it is a text file we index.
pub fn text_of(bytes: &[u8]) -> Result<&str, Skip> {
    if bytes.len() > MAX_FILE_BYTES {
        return Err(Skip::TooLarge);
    }
    if bytes.contains(&0) {
        return Err(Skip::NotText);
    }
    let text = std::str::from_utf8(bytes).map_err(|_| Skip::NotText)?;
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    if text.trim().is_empty() {
        return Err(Skip::Empty);
    }
    Ok(text)
}

/// A line cut into pieces of at most [`CHUNK_CHARS`] characters (minified files have huge lines).
fn pieces(line: &str) -> Vec<&str> {
    if line.chars().count() <= CHUNK_CHARS {
        return vec![line];
    }
    let mut out = Vec::new();
    let mut rest = line;
    while !rest.is_empty() {
        let end = rest
            .char_indices()
            .nth(CHUNK_CHARS)
            .map_or(rest.len(), |(i, _)| i);
        out.push(&rest[..end]);
        rest = &rest[end..];
    }
    out
}

/// Splits `text` into chunks of whole lines (see the constants), with [`OVERLAP_LINES`] lines of
/// overlap. Line numbers are those of the file (1-based). Blank-only chunks are dropped.
pub fn chunk(text: &str) -> Vec<Chunk> {
    // (line number, piece of that line)
    let lines: Vec<(u32, &str)> = text
        .lines()
        .enumerate()
        .flat_map(|(i, l)| {
            let n = u32::try_from(i + 1).unwrap_or(u32::MAX);
            pieces(l).into_iter().map(move |p| (n, p))
        })
        .collect();
    let mut out = Vec::new();
    let mut start = 0;
    while start < lines.len() {
        let mut end = start;
        let mut chars = 0;
        while end < lines.len() && end - start < CHUNK_LINES {
            let c = lines[end].1.chars().count() + 1;
            if end > start && chars + c > CHUNK_CHARS {
                break;
            }
            chars += c;
            end += 1;
        }
        let slice = &lines[start..end];
        let text = slice.iter().map(|(_, l)| *l).collect::<Vec<_>>().join("\n");
        if !text.trim().is_empty() {
            out.push(Chunk {
                start_line: slice[0].0,
                end_line: slice[slice.len() - 1].0,
                text,
            });
        }
        if end >= lines.len() {
            break;
        }
        // Step forward, keeping some overlap, but always make progress.
        start = end.saturating_sub(OVERLAP_LINES).max(start + 1);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chunks_carry_file_line_numbers_and_overlap() {
        let text: String = (1..=100).map(|i| format!("line {i}\n")).collect();
        let c = chunk(&text);
        assert_eq!(c[0].start_line, 1);
        assert_eq!(c[0].end_line, 40);
        assert!(c[0].text.starts_with("line 1\n"));
        assert!(c[0].text.ends_with("line 40"));
        // Overlap: the next chunk starts OVERLAP_LINES before the previous end.
        assert_eq!(c[1].start_line, 37);
        assert_eq!(c.last().unwrap().end_line, 100);
        for w in c.windows(2) {
            assert!(w[1].start_line > w[0].start_line, "always progresses");
        }
    }

    #[test]
    fn long_lines_are_cut_but_keep_their_line_number() {
        let long = "x".repeat(CHUNK_CHARS * 2 + 10);
        let text = format!("first\n{long}\nlast");
        let c = chunk(&text);
        assert!(
            c.iter()
                .all(|x| x.text.chars().count() <= CHUNK_CHARS + CHUNK_LINES)
        );
        assert!(c.iter().any(|x| x.start_line == 2 && x.end_line == 2));
        assert_eq!(c.last().unwrap().end_line, 3);
    }

    #[test]
    fn only_text_files_are_indexed() {
        assert_eq!(text_of(b"hello\n"), Ok("hello\n"));
        assert_eq!(text_of("\u{feff}bom".as_bytes()), Ok("bom"));
        assert_eq!(text_of(b"a\0b"), Err(Skip::NotText));
        assert_eq!(text_of(&[0xff, 0xfe, 0x41]), Err(Skip::NotText));
        assert_eq!(text_of(b"  \n\t"), Err(Skip::Empty));
        assert_eq!(
            text_of(&vec![b'a'; MAX_FILE_BYTES + 1]),
            Err(Skip::TooLarge)
        );
    }

    #[test]
    fn small_and_blank_files() {
        assert_eq!(chunk("one line").len(), 1);
        assert!(chunk("\n\n\n").is_empty());
        let c = chunk("a\n\n\nb");
        assert_eq!((c[0].start_line, c[0].end_line), (1, 4));
    }
}
