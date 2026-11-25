//! Chat skills (T-SKL-01): instructions plus a tool allowlist that the user picks for a
//! chat, in a `SKILL.md` file.
//!
//! These are not the tool-like skills of [`crate::manifest`] (`skill.yaml`, which the model
//! calls). A chat skill changes how the assistant works in one chat: its instructions are
//! added to the system prompt, and its `allowed-tools` narrows the tools on offer.
//!
//! ```text
//! skills/library/<name>/SKILL.md
//! ---
//! name: proofread
//! description: Fixes spelling and grammar without changing the meaning.
//! allowed-tools: []
//! ---
//! Instructions in Markdown…
//! ```
//!
//! The front matter follows the Agent Skills `SKILL.md` layout (`name`, `description`,
//! `allowed-tools`, `license`), with one difference on purpose: here `allowed-tools` only
//! narrows. It never pre-approves a tool, so approvals, risk tiers and the user's per-tool
//! choices always apply. Unknown keys are refused, so a file can't add a field that looks
//! like it grants more.

use crate::manifest::{ManifestError, find_files};
use serde::Deserialize;
use std::path::Path;

/// The file name a chat skill's folder holds.
pub const SKILL_FILE: &str = "SKILL.md";
/// Longest instructions accepted, in characters (they go into every request of the chat).
pub const MAX_INSTRUCTIONS: usize = 16_000;
const MAX_NAME: usize = 64;
const MAX_DESCRIPTION: usize = 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChatSkill {
    /// Lowercase letters, digits and single hyphens; equals the folder name.
    pub name: String,
    /// When to use it (shown in the picker).
    pub description: String,
    /// Capability-id patterns, as in an agent manifest's `tools` (`artifact.create`,
    /// `mcp.files.*`). Empty means no tools.
    pub allowed_tools: Vec<String>,
    /// SPDX id of the skill text's licence, if stated.
    pub license: Option<String>,
    /// The Markdown body.
    pub instructions: String,
}

impl ChatSkill {
    /// "meeting-notes" → "Meeting notes".
    pub fn label(&self) -> String {
        let words = self.name.replace('-', " ");
        let mut c = words.chars();
        match c.next() {
            Some(f) => f.to_uppercase().chain(c).collect(),
            None => String::new(),
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FrontMatter {
    name: String,
    description: String,
    #[serde(rename = "allowed-tools")]
    allowed_tools: Vec<String>,
    #[serde(default)]
    license: Option<String>,
}

/// Whether `p` is a tool pattern: lowercase letters, digits, `.`, `_` and `-`, with an
/// optional trailing `*` (a lone `*` means every tool the assistant already has).
pub fn valid_tool_pattern(p: &str) -> bool {
    let body = p.strip_suffix('*').unwrap_or(p);
    !p.is_empty()
        && p.len() <= 200
        && body
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '.' | '_' | '-'))
}

fn valid_name(n: &str) -> bool {
    !n.is_empty()
        && n.len() <= MAX_NAME
        && !n.starts_with('-')
        && !n.ends_with('-')
        && !n.contains("--")
        && n.chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

/// Parses a `SKILL.md`. `folder` is the name of the folder it sits in, which `name` must
/// match (pass `None` when there is no folder, e.g. in tests).
pub fn parse(file: &Path, text: &str, folder: Option<&str>) -> Result<ChatSkill, ManifestError> {
    let err = |path: &str, msg: String| ManifestError::one(file, path, msg);
    let text = text
        .strip_prefix('\u{feff}')
        .unwrap_or(text)
        .replace("\r\n", "\n");
    let rest = text
        .strip_prefix("---\n")
        .ok_or_else(|| err("", "must start with a `---` front matter block".into()))?;
    let end = rest
        .find("\n---\n")
        .or_else(|| rest.strip_suffix("\n---").map(|r| r.len()))
        .ok_or_else(|| err("", "the front matter has no closing `---`".into()))?;
    let (yaml, body) = (&rest[..end], rest[end + 4..].trim_start_matches('\n'));
    let fm: FrontMatter =
        serde_yaml_ng::from_str(yaml).map_err(|e| err("/front matter", e.to_string()))?;

    if !valid_name(&fm.name) {
        return Err(err(
            "/name",
            format!(
                "`{}` must be lowercase letters, digits and single hyphens, at most {MAX_NAME} characters",
                fm.name
            ),
        ));
    }
    if let Some(folder) = folder
        && folder != fm.name
    {
        return Err(err(
            "/name",
            format!("`{}` must match its folder name `{folder}`", fm.name),
        ));
    }
    let description = fm.description.trim().to_owned();
    if description.is_empty() || description.chars().count() > MAX_DESCRIPTION {
        return Err(err(
            "/description",
            format!("must be 1 to {MAX_DESCRIPTION} characters"),
        ));
    }
    for (i, p) in fm.allowed_tools.iter().enumerate() {
        if !valid_tool_pattern(p) {
            return Err(err(
                &format!("/allowed-tools/{i}"),
                format!(
                    "`{p}` isn't a tool pattern: use a tool id such as `artifact.create`, or a \
                     prefix ending in `*` such as `mcp.files.*`"
                ),
            ));
        }
    }
    let instructions = body.trim().to_owned();
    if instructions.is_empty() {
        return Err(err("/instructions", "the instructions are empty".into()));
    }
    if instructions.chars().count() > MAX_INSTRUCTIONS {
        return Err(err(
            "/instructions",
            format!("longer than {MAX_INSTRUCTIONS} characters"),
        ));
    }
    Ok(ChatSkill {
        name: fm.name,
        description,
        allowed_tools: fm.allowed_tools,
        license: fm
            .license
            .map(|l| l.trim().to_owned())
            .filter(|l| !l.is_empty()),
        instructions,
    })
}

/// Loads one `SKILL.md`.
pub fn load(file: &Path) -> Result<ChatSkill, ManifestError> {
    let text = std::fs::read_to_string(file)
        .map_err(|e| ManifestError::one(file, "", format!("can't read: {e}")))?;
    let folder = file
        .parent()
        .and_then(|d| d.file_name())
        .and_then(|n| n.to_str());
    parse(file, &text, folder)
}

/// Every `SKILL.md` under `dir`, sorted by name. Broken files are returned as errors next
/// to the skills that loaded, so one bad file doesn't hide the rest.
pub fn load_dir(dir: &Path) -> (Vec<ChatSkill>, Vec<ManifestError>) {
    let mut skills = Vec::new();
    let mut errors = Vec::new();
    for f in find_files(dir, SKILL_FILE) {
        match load(&f) {
            Ok(s) => skills.push(s),
            Err(e) => errors.push(e),
        }
    }
    skills.sort_by(|a, b| a.name.cmp(&b.name));
    (skills, errors)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(text: &str) -> Result<ChatSkill, ManifestError> {
        parse(Path::new("SKILL.md"), text, Some("proofread"))
    }

    const OK: &str = "---\nname: proofread\ndescription: Fix spelling.\nallowed-tools: []\n---\n\nCheck the text.\n";

    #[test]
    fn parses_front_matter_and_body() {
        let s = p(OK).unwrap();
        assert_eq!(s.name, "proofread");
        assert_eq!(s.description, "Fix spelling.");
        assert!(s.allowed_tools.is_empty());
        assert_eq!(s.instructions, "Check the text.");
        assert_eq!(s.label(), "Proofread");
        // CRLF and a BOM are fine.
        assert_eq!(
            p(&format!("\u{feff}{}", OK.replace('\n', "\r\n"))).unwrap(),
            s
        );
    }

    #[test]
    fn refuses_fields_that_could_widen_approvals() {
        for extra in ["auto-approve: true", "approval: auto", "risk: read"] {
            let t = OK.replace("allowed-tools: []", &format!("allowed-tools: []\n{extra}"));
            assert!(p(&t).is_err(), "{extra}");
        }
    }

    #[test]
    fn refuses_bad_files() {
        let cases = [
            ("no front matter", "Check the text.".to_string()),
            ("unclosed", "---\nname: proofread\n".to_string()),
            (
                "missing allowed-tools",
                OK.replace("allowed-tools: []\n", ""),
            ),
            (
                "bad name",
                OK.replace("name: proofread", "name: Proof_Read"),
            ),
            (
                "name ≠ folder",
                OK.replace("name: proofread", "name: other"),
            ),
            ("empty description", OK.replace("Fix spelling.", "''")),
            ("bad pattern", OK.replace("[]", "['mcp.*.read']")),
            (
                "empty body",
                "---\nname: proofread\ndescription: x\nallowed-tools: []\n---\n  \n".into(),
            ),
            (
                "too long",
                format!("{}{}", OK, "x".repeat(MAX_INSTRUCTIONS)),
            ),
        ];
        for (why, t) in cases {
            assert!(p(&t).is_err(), "{why}");
        }
    }

    #[test]
    fn labels_read_as_words() {
        let mut s = p(OK).unwrap();
        s.name = "meeting-notes".into();
        assert_eq!(s.label(), "Meeting notes");
    }
}
