//! A fetched page's main text, as markdown (#369).
//!
//! HTML goes through `dom_smoothie`, a port of Mozilla's Readability: it finds the article
//! and drops navigation, ads and footers, then writes markdown. Plain text and markdown pass
//! through. The result is capped at [`MAX_MARKDOWN_CHARS`], with a note when it was cut.
//!
//! The text is untrusted: whoever puts it in a prompt wraps it with
//! `dn_core::untrusted::block`.

use crate::fetch::Fetched;

/// The most characters of a page's text kept.
pub const MAX_MARKDOWN_CHARS: usize = 60_000;

/// A page ready to read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Page {
    /// The address asked for.
    pub url: String,
    /// Where the page came from after redirects.
    pub final_url: String,
    /// Its title, if it has one.
    pub title: Option<String>,
    /// Its author, if the page names one.
    pub byline: Option<String>,
    /// When it was published, as the page says.
    pub published: Option<String>,
    /// Its main text, as markdown.
    pub markdown: String,
    /// The main text was longer than [`MAX_MARKDOWN_CHARS`] and was cut.
    pub cut: bool,
}

/// Why a page has no text to read.
#[derive(Debug, thiserror::Error)]
pub enum ExtractError {
    #[error("the page has no readable text: {0}")]
    NotReadable(String),
}

fn cap(text: &str) -> (String, bool) {
    let total = text.chars().count();
    if total <= MAX_MARKDOWN_CHARS {
        return (text.to_owned(), false);
    }
    let mut out: String = text.chars().take(MAX_MARKDOWN_CHARS).collect();
    out.push_str(&format!(
        "\n\n[cut: {} more characters not shown]",
        total - MAX_MARKDOWN_CHARS
    ));
    (out, true)
}

fn non_empty(s: Option<String>) -> Option<String> {
    s.map(|s| s.trim().to_owned()).filter(|s| !s.is_empty())
}

/// The main text of `page`.
pub fn extract(page: &Fetched) -> Result<Page, ExtractError> {
    let (title, byline, published, text) = match page.content_type.as_str() {
        "text/html" | "application/xhtml+xml" => {
            let config = dom_smoothie::Config {
                text_mode: dom_smoothie::TextMode::Markdown,
                ..Default::default()
            };
            let mut r = dom_smoothie::Readability::new(
                page.text.as_str(),
                Some(&page.final_url),
                Some(config),
            )
            .map_err(|e| ExtractError::NotReadable(e.to_string()))?;
            let a = r
                .parse()
                .map_err(|e| ExtractError::NotReadable(e.to_string()))?;
            (
                non_empty(Some(a.title)),
                non_empty(a.byline),
                non_empty(a.published_time),
                a.text_content.to_string(),
            )
        }
        _ => (None, None, None, page.text.clone()),
    };
    if text.trim().is_empty() {
        return Err(ExtractError::NotReadable("it's empty".into()));
    }
    let (markdown, cut) = cap(text.trim());
    Ok(Page {
        url: page.url.clone(),
        final_url: page.final_url.clone(),
        title,
        byline,
        published,
        markdown,
        cut,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fetched(ctype: &str, text: &str) -> Fetched {
        Fetched {
            url: "https://news.example.org/a".into(),
            final_url: "https://news.example.org/a".into(),
            content_type: ctype.into(),
            text: text.into(),
        }
    }

    const ARTICLE: &str = r#"<!doctype html><html><head><title>Tide tables | Coast News</title>
<meta name="author" content="Rosa Alvarez"></head><body>
<nav><a href="/">Home</a> <a href="/sports">Sports</a> <a href="/weather">Weather</a></nav>
<article><h1>Tide tables</h1>
<p>The tide tables for the coming month were published on Monday by the harbour office, and
they show two unusually high tides in the second week, both in the early morning.</p>
<p>Boat owners are asked to check their moorings before then. The harbour office will keep
its doors open an hour longer on both days for anyone who needs help.</p>
<p>More details, including a <a href="https://harbour.example.org/tides">full table</a>, are
on the harbour office's site.</p></article>
<footer>Copyright Coast News. Subscribe now!</footer></body></html>"#;

    #[test]
    fn an_article_becomes_markdown_without_navigation_or_footer() {
        let p = extract(&fetched("text/html", ARTICLE)).unwrap();
        assert!(
            p.markdown.contains("two unusually high tides"),
            "{}",
            p.markdown
        );
        assert!(
            p.markdown
                .contains("[full table](https://harbour.example.org/tides)"),
            "{}",
            p.markdown
        );
        assert!(!p.markdown.contains("Subscribe now"), "{}", p.markdown);
        assert!(!p.markdown.contains("Sports"), "{}", p.markdown);
        assert!(p.title.unwrap().contains("Tide tables"));
        assert!(!p.cut);
    }

    #[test]
    fn plain_text_passes_through_and_long_text_is_cut_with_a_note() {
        let p = extract(&fetched("text/plain", "  hello  ")).unwrap();
        assert_eq!(p.markdown, "hello");
        let long = "a".repeat(MAX_MARKDOWN_CHARS + 5);
        let p = extract(&fetched("text/markdown", &long)).unwrap();
        assert!(p.cut);
        assert!(p.markdown.ends_with("[cut: 5 more characters not shown]"));
    }

    #[test]
    fn an_empty_page_has_nothing_to_read() {
        assert!(extract(&fetched("text/plain", "   ")).is_err());
        assert!(extract(&fetched("text/html", "<html><body></body></html>")).is_err());
    }
}
