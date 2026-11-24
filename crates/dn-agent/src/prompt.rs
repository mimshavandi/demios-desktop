//! Prompt registry (T-AGT-03): versioned prompts on disk, `{placeholder}` rendering.
//!
//! Layout: `<root>/**/manifest.yaml` + the version files it names (e.g. `v1.md`).
//! Refs: `text.summarize` (active version) or `text.summarize@v1` (pinned).
//!
//! Template syntax: `{name}` is a placeholder (`name` = letters, digits, `_`, not
//! starting with a digit); `{{` and `}}` are literal braces. Any other `{` is kept as
//! is, so JSON examples in prompts need no escaping.

use crate::manifest::{ManifestError, PromptManifest, find_files, load_prompt_manifest};
use crate::schema::{is_dotted_id, is_version};
use std::collections::{BTreeSet, HashMap};
use std::path::Path;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PromptError {
    #[error("invalid prompt ref `{0}` (expected `id` or `id@vN`)")]
    BadRef(String),
    #[error("unknown prompt `{0}`")]
    UnknownPrompt(String),
    #[error("prompt `{id}` has no version `{version}` (available: {available})")]
    UnknownVersion {
        id: String,
        version: String,
        available: String,
    },
    #[error("prompt `{prompt}` needs variable `{name}`")]
    MissingVariable { prompt: String, name: String },
    #[error("{0}")]
    Manifest(#[from] ManifestError),
}

/// One loaded prompt version.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Prompt {
    pub id: String,
    pub version: String,
    pub template: String,
    /// Placeholders used by the template, in first-use order.
    pub variables: Vec<String>,
}

impl Prompt {
    /// `id@version`, e.g. for logging which prompt produced an answer.
    pub fn reference(&self) -> String {
        format!("{}@{}", self.id, self.version)
    }

    /// Fills every placeholder; a placeholder without a value is an error.
    pub fn render<V: AsRef<str>>(&self, vars: &HashMap<&str, V>) -> Result<String, PromptError> {
        let mut out = String::with_capacity(self.template.len());
        for tok in tokens(&self.template) {
            match tok {
                Token::Text(t) => out.push_str(t),
                Token::Var(name) => match vars.get(name) {
                    Some(v) => out.push_str(v.as_ref()),
                    None => {
                        return Err(PromptError::MissingVariable {
                            prompt: self.reference(),
                            name: name.to_string(),
                        });
                    }
                },
            }
        }
        Ok(out)
    }
}

enum Token<'a> {
    Text(&'a str),
    Var(&'a str),
}

fn tokens(t: &str) -> Vec<Token<'_>> {
    let mut out = Vec::new();
    let b = t.as_bytes();
    let (mut i, mut start) = (0, 0);
    while i < b.len() {
        let two = |c: u8| i + 1 < b.len() && b[i] == c && b[i + 1] == c;
        if two(b'{') || two(b'}') {
            out.push(Token::Text(&t[start..i + 1]));
            i += 2;
            start = i;
        } else if b[i] == b'{' {
            let rest = &t[i + 1..];
            let end = rest.find('}');
            match end.map(|e| &rest[..e]) {
                Some(name) if crate::schema::is_identifier(name) => {
                    out.push(Token::Text(&t[start..i]));
                    out.push(Token::Var(name));
                    i += name.len() + 2;
                    start = i;
                }
                _ => i += 1,
            }
        } else {
            i += 1;
        }
    }
    out.push(Token::Text(&t[start..]));
    out
}

fn placeholders(template: &str) -> Vec<String> {
    let mut seen = BTreeSet::new();
    let mut out = Vec::new();
    for tok in tokens(template) {
        if let Token::Var(n) = tok
            && seen.insert(n)
        {
            out.push(n.to_string());
        }
    }
    out
}

struct Entry {
    manifest: PromptManifest,
    versions: HashMap<String, Prompt>,
}

#[derive(Default)]
pub struct PromptRegistry {
    prompts: HashMap<String, Entry>,
}

impl PromptRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Loads every `manifest.yaml` under `root`. Fails with all problems found.
    pub fn load_dir(root: &Path) -> Result<Self, Vec<PromptError>> {
        let mut reg = Self::new();
        let mut errors = Vec::new();
        for file in find_files(root, "manifest.yaml") {
            if let Err(e) = reg.load_manifest(&file) {
                errors.push(e);
            }
        }
        if errors.is_empty() {
            Ok(reg)
        } else {
            Err(errors)
        }
    }

    /// Loads one manifest and its version files.
    pub fn load_manifest(&mut self, file: &Path) -> Result<(), PromptError> {
        let manifest = load_prompt_manifest(file)?;
        let dir = file.parent().unwrap_or(Path::new("."));
        let mut versions = HashMap::new();
        for (i, v) in manifest.versions.iter().enumerate() {
            let path = dir.join(&v.file);
            let template = std::fs::read_to_string(&path).map_err(|e| {
                ManifestError::one(
                    file,
                    &format!("/versions/{i}/file"),
                    format!("can't read `{}`: {e}", v.file),
                )
            })?;
            let used = placeholders(&template);
            if let Some(declared) = &v.variables {
                let undeclared: Vec<&String> =
                    used.iter().filter(|u| !declared.contains(u)).collect();
                if !undeclared.is_empty() {
                    let list: Vec<String> = undeclared.iter().map(|u| format!("`{u}`")).collect();
                    return Err(ManifestError::one(
                        file,
                        &format!("/versions/{i}/variables"),
                        format!(
                            "`{}` uses undeclared placeholder(s) {}",
                            v.file,
                            list.join(", ")
                        ),
                    )
                    .into());
                }
            }
            versions.insert(
                v.version.clone(),
                Prompt {
                    id: manifest.id.clone(),
                    version: v.version.clone(),
                    template,
                    variables: used,
                },
            );
        }
        if self.prompts.contains_key(&manifest.id) {
            return Err(ManifestError::one(
                file,
                "/id",
                format!("duplicate prompt id `{}`", manifest.id),
            )
            .into());
        }
        self.prompts
            .insert(manifest.id.clone(), Entry { manifest, versions });
        Ok(())
    }

    /// `id` → active version; `id@vN` → that version.
    pub fn load_prompt(&self, reference: &str) -> Result<&Prompt, PromptError> {
        let (id, version) = match reference.split_once('@') {
            Some((id, v)) if is_dotted_id(id) && is_version(v) => (id, Some(v)),
            None if is_dotted_id(reference) => (reference, None),
            _ => return Err(PromptError::BadRef(reference.into())),
        };
        let entry = self
            .prompts
            .get(id)
            .ok_or_else(|| PromptError::UnknownPrompt(id.into()))?;
        let version = version.unwrap_or(&entry.manifest.active_version);
        entry.versions.get(version).ok_or_else(|| {
            let mut available: Vec<&str> = entry.versions.keys().map(String::as_str).collect();
            available.sort();
            PromptError::UnknownVersion {
                id: id.into(),
                version: version.into(),
                available: available.join(", "),
            }
        })
    }

    pub fn active_version(&self, id: &str) -> Option<&str> {
        self.prompts
            .get(id)
            .map(|e| e.manifest.active_version.as_str())
    }

    pub fn ids(&self) -> Vec<&str> {
        let mut v: Vec<&str> = self.prompts.keys().map(String::as_str).collect();
        v.sort();
        v
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn prompt(t: &str) -> Prompt {
        Prompt {
            id: "t.p".into(),
            version: "v1".into(),
            template: t.into(),
            variables: placeholders(t),
        }
    }

    #[test]
    fn renders_and_escapes() {
        let p = prompt("Hi {name}! JSON: {\"a\": 1} and {{name}} and { name } and {name}.");
        assert_eq!(p.variables, vec!["name"]);
        let vars = HashMap::from([("name", "Ada")]);
        assert_eq!(
            p.render(&vars).unwrap(),
            "Hi Ada! JSON: {\"a\": 1} and {name} and { name } and Ada."
        );
    }

    #[test]
    fn missing_variable_is_an_error() {
        let p = prompt("Summarise {text} in {max_words} words.");
        let err = p.render(&HashMap::from([("text", "...")])).unwrap_err();
        assert_eq!(
            err,
            PromptError::MissingVariable {
                prompt: "t.p@v1".into(),
                name: "max_words".into()
            }
        );
        assert_eq!(
            err.to_string(),
            "prompt `t.p@v1` needs variable `max_words`"
        );
    }

    #[test]
    fn loads_versions_from_disk() {
        let dir = std::env::temp_dir().join(format!("dn-agent-prompts-{}", std::process::id()));
        let p = dir.join("greet");
        std::fs::create_dir_all(&p).unwrap();
        std::fs::write(
            p.join("manifest.yaml"),
            "id: demo.greet\nactive_version: v2\nversions:\n  - {version: v1, file: v1.md, variables: [name]}\n  - {version: v2, file: v2.md}\n",
        )
        .unwrap();
        std::fs::write(p.join("v1.md"), "Hello {name}").unwrap();
        std::fs::write(p.join("v2.md"), "Hey {name}, {mood}?").unwrap();
        let reg = PromptRegistry::load_dir(&dir).unwrap();
        assert_eq!(reg.active_version("demo.greet"), Some("v2"));
        assert_eq!(reg.load_prompt("demo.greet").unwrap().version, "v2");
        assert_eq!(
            reg.load_prompt("demo.greet@v1").unwrap().template,
            "Hello {name}"
        );
        assert!(
            matches!(reg.load_prompt("demo.greet@v9"), Err(PromptError::UnknownVersion { ref available, .. }) if available == "v1, v2")
        );
        assert!(matches!(
            reg.load_prompt("nope"),
            Err(PromptError::UnknownPrompt(_))
        ));
        assert!(matches!(
            reg.load_prompt("Bad@x"),
            Err(PromptError::BadRef(_))
        ));
        // Undeclared placeholder is caught at load time.
        std::fs::write(p.join("v1.md"), "Hello {name} {extra}").unwrap();
        let errs = PromptRegistry::load_dir(&dir).err().unwrap();
        assert!(
            errs[0]
                .to_string()
                .contains("undeclared placeholder(s) `extra`"),
            "{}",
            errs[0]
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
