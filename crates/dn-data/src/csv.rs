//! CSV and other delimited text (RFC 4180 quoting), with the delimiter and the encoding
//! detected.

use crate::DataError;
use crate::table::{Cell, Limits, Table, build};

/// Decodes the file: UTF-8 (BOM dropped), UTF-16 with a BOM, otherwise Windows-1252 (what
/// Excel writes on Western Windows). Returns the text and, for anything but UTF-8, a note.
pub fn decode(bytes: &[u8]) -> (String, Option<&'static str>) {
    if let Some((enc, bom)) = encoding_rs::Encoding::for_bom(bytes) {
        let (text, _) = enc.decode_without_bom_handling(&bytes[bom..]);
        let note = (enc != encoding_rs::UTF_8).then_some(if enc == encoding_rs::UTF_16LE {
            "UTF-16 (little-endian)"
        } else {
            "UTF-16 (big-endian)"
        });
        return (text.into_owned(), note);
    }
    match std::str::from_utf8(bytes) {
        Ok(s) => (s.to_owned(), None),
        Err(_) => {
            let (text, _, _) = encoding_rs::WINDOWS_1252.decode(bytes);
            (text.into_owned(), Some("Windows-1252"))
        }
    }
}

const DELIMITERS: [(char, &str); 4] = [
    (',', "comma"),
    (';', "semicolon"),
    ('\t', "tab"),
    ('|', "vertical bar"),
];

/// The delimiter: the candidate that splits the first lines into the same number of fields
/// (more than one) most consistently. Quotes are respected. Comma when nothing fits.
pub fn sniff_delimiter(text: &str) -> char {
    let mut best = (',', 0usize, 0usize); // (delimiter, rows agreeing, fields)
    for (d, _) in DELIMITERS {
        let rows = parse(text, d, 50);
        if rows.is_empty() {
            continue;
        }
        let mut counts = std::collections::HashMap::<usize, usize>::new();
        for r in &rows {
            *counts.entry(r.len()).or_default() += 1;
        }
        let (fields, agreeing) = counts
            .into_iter()
            .max_by_key(|(f, n)| (*n, *f))
            .unwrap_or((0, 0));
        if fields > 1 && (agreeing, fields) > (best.1, best.2) {
            best = (d, agreeing, fields);
        }
    }
    best.0
}

/// RFC 4180 rows: fields split by `delim`; `"` quotes a field, `""` inside quotes is a quote;
/// quoted fields may hold the delimiter and line breaks; CRLF and LF both end a row. At most
/// `max_rows` rows (`usize::MAX` for all).
pub fn parse(text: &str, delim: char, max_rows: usize) -> Vec<Vec<String>> {
    let mut rows = Vec::new();
    let mut row = Vec::new();
    let mut field = String::new();
    let mut quoted = false;
    let mut at_start = true; // at the start of a field
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if quoted {
            if c == '"' {
                if chars.peek() == Some(&'"') {
                    field.push('"');
                    chars.next();
                } else {
                    quoted = false;
                }
            } else {
                field.push(c);
            }
            continue;
        }
        match c {
            '"' if at_start => {
                quoted = true;
                at_start = false;
            }
            c if c == delim => {
                row.push(std::mem::take(&mut field));
                at_start = true;
            }
            '\r' if chars.peek() == Some(&'\n') => {}
            '\n' | '\r' => {
                row.push(std::mem::take(&mut field));
                rows.push(std::mem::take(&mut row));
                at_start = true;
                if rows.len() >= max_rows {
                    return rows;
                }
            }
            c => {
                field.push(c);
                at_start = false;
            }
        }
    }
    if !field.is_empty() || !row.is_empty() {
        row.push(field);
        rows.push(row);
    }
    rows
}

/// Reads a delimited text file into a table.
pub fn read(bytes: &[u8], limits: &Limits) -> Result<Table, DataError> {
    if bytes.len() as u64 > limits.max_bytes {
        return Err(DataError::TooLarge(limits.max_bytes));
    }
    let (text, enc_note) = decode(bytes);
    let delim = sniff_delimiter(&text);
    let rows = parse(&text, delim, usize::MAX);
    if rows.iter().all(|r| r.iter().all(|f| f.trim().is_empty())) {
        return Err(DataError::Empty);
    }
    let total = rows.len() as u64;
    let name = DELIMITERS
        .iter()
        .find(|(d, _)| *d == delim)
        .map(|(_, n)| *n)
        .unwrap_or("comma");
    let mut notes = vec![format!("{name}-separated")];
    if let Some(e) = enc_note {
        notes.push(format!("text encoding {e}"));
    }
    let cells = rows
        .into_iter()
        .map(|r| {
            r.into_iter()
                .map(|f| {
                    if f.trim().is_empty() {
                        Cell::Empty
                    } else {
                        Cell::Text(f)
                    }
                })
                .collect()
        })
        .collect();
    Ok(build(None, cells, Some(total), limits, notes))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quotes_doubled_quotes_and_line_breaks_inside_fields() {
        let rows = parse(
            "a,b\r\n\"x, y\",\"He said \"\"hi\"\"\nthen left\"\n1,2",
            ',',
            usize::MAX,
        );
        assert_eq!(
            rows,
            vec![
                vec!["a".to_string(), "b".into()],
                vec!["x, y".into(), "He said \"hi\"\nthen left".into()],
                vec!["1".into(), "2".into()],
            ]
        );
    }

    #[test]
    fn the_delimiter_is_detected() {
        assert_eq!(sniff_delimiter("a;b;c\n1,5;2,5;3\n4;5;6\n"), ';');
        assert_eq!(sniff_delimiter("a\tb\n1\t2\n"), '\t');
        assert_eq!(
            sniff_delimiter("name,note\n\"Lee; Kim\",x\n\"A; B\",y\n"),
            ','
        );
        assert_eq!(sniff_delimiter("just one column\nand another line\n"), ',');
    }

    #[test]
    fn encodings_are_detected() {
        let (t, note) = decode(b"\xEF\xBB\xBFStadt;Gr\xC3\xB6\xC3\x9Fe");
        assert_eq!((t.as_str(), note), ("Stadt;Größe", None));
        let (t, note) = decode(b"Stadt;Gr\xF6\xDFe");
        assert_eq!((t.as_str(), note), ("Stadt;Größe", Some("Windows-1252")));
        let (t, note) = decode(b"\xFF\xFEa\x00,\x00b\x00");
        assert_eq!((t.as_str(), note), ("a,b", Some("UTF-16 (little-endian)")));
    }

    #[test]
    fn empty_and_too_large_files_are_refused() {
        assert!(matches!(
            read(b" \n\n", &Limits::default()),
            Err(DataError::Empty)
        ));
        let tiny = Limits {
            max_bytes: 3,
            ..Limits::default()
        };
        assert!(matches!(
            read(b"a,b,c\n", &tiny),
            Err(DataError::TooLarge(3))
        ));
    }
}
