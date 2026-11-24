//! `agent.yaml`, `skill.yaml` and prompt `manifest.yaml` (T-AGT-01, ADR-0005).
//!
//! Loading = parse YAML → validate against `schemas/*.schema.json` → deserialize →
//! cross-field checks. Every problem is reported with file and JSON-pointer path, all
//! at once, so authors fix a manifest in one pass.

use crate::schema::{self, Problem};
use dn_core::capability::RiskTier;
use dn_core::chat::{Reasoning, ToolSpec};
use dn_core::model::ModelSlot;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

pub const AGENT_SCHEMA: &str = include_str!("../schemas/agent.schema.json");
pub const SKILL_SCHEMA: &str = include_str!("../schemas/skill.schema.json");
pub const PROMPT_SCHEMA: &str = include_str!("../schemas/prompt-manifest.schema.json");

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AgentManifest {
    pub id: String,
    pub version: u32,
    pub description: String,
    /// Prompt ref: `id` or `id@vN`.
    pub prompt: String,
    pub model_slots: Vec<ModelSlot>,
    #[serde(default)]
    pub skills: Vec<String>,
    /// Allowed capability ids; trailing `*` matches a prefix.
    #[serde(default)]
    pub tools: Vec<String>,
    #[serde(default)]
    pub policies: AgentPolicies,
}

impl AgentManifest {
    pub fn slot(&self, slot_id: &str) -> Option<&ModelSlot> {
        self.model_slots.iter().find(|s| s.slot_id == slot_id)
    }

    /// Whether a capability id is allowed by `tools` (exact or `prefix.*`).
    pub fn allows_tool(&self, capability_id: &str) -> bool {
        self.tools.iter().any(|p| match p.strip_suffix('*') {
            Some(prefix) => capability_id.starts_with(prefix),
            None => p == capability_id,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AgentPolicies {
    #[serde(default = "default_max_steps")]
    pub max_steps: u32,
    #[serde(default)]
    pub reasoning: Reasoning,
    #[serde(default = "default_caller")]
    pub caller: String,
    #[serde(default)]
    pub max_tokens: Option<u32>,
    #[serde(default)]
    pub temperature: Option<f32>,
}

impl Default for AgentPolicies {
    fn default() -> Self {
        Self {
            max_steps: default_max_steps(),
            reasoning: Reasoning::Auto,
            caller: default_caller(),
            max_tokens: None,
            temperature: None,
        }
    }
}

fn default_max_steps() -> u32 {
    8
}

fn default_caller() -> String {
    "chat.reply".into()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SkillKind {
    Prompt,
    ToolChain,
    Builtin,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ChainStep {
    pub tool: String,
    #[serde(default)]
    pub arguments: serde_json::Map<String, Value>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SkillManifest {
    pub id: String,
    pub version: u32,
    pub description: String,
    pub kind: SkillKind,
    pub risk: RiskTier,
    pub input_schema: Value,
    #[serde(default)]
    pub output_schema: Option<Value>,
    #[serde(default)]
    pub prompt: Option<String>,
    #[serde(default)]
    pub model_slot: Option<String>,
    #[serde(default)]
    pub steps: Vec<ChainStep>,
    #[serde(default)]
    pub handler: Option<String>,
}

impl SkillManifest {
    /// Provider-safe tool name: function names may not contain dots
    /// (OpenAI/Gemini allow `[A-Za-z0-9_-]{1,64}`), so `text.summarize` → `text__summarize`.
    pub fn tool_name(&self) -> String {
        tool_name(&self.id)
    }

    pub fn tool_spec(&self) -> ToolSpec {
        ToolSpec {
            name: self.tool_name(),
            description: self.description.clone(),
            input_schema: self.input_schema.clone(),
        }
    }
}

/// Dotted id → provider-safe tool name (`.` → `__`). See [`SkillManifest::tool_name`].
pub fn tool_name(dotted_id: &str) -> String {
    dotted_id.replace('.', "__")
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PromptManifest {
    pub id: String,
    #[serde(default)]
    pub description: Option<String>,
    pub active_version: String,
    pub versions: Vec<PromptVersion>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PromptVersion {
    pub version: String,
    pub file: String,
    /// Declared placeholders. When present, the template may use only these.
    #[serde(default)]
    pub variables: Option<Vec<String>>,
    #[serde(default)]
    pub description: Option<String>,
}

/// Every problem found in one manifest file.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub struct ManifestError {
    pub file: PathBuf,
    pub problems: Vec<Problem>,
}

impl fmt::Display for ManifestError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let n = self.problems.len();
        write!(
            f,
            "{}: {n} problem{}",
            self.file.display(),
            if n == 1 { "" } else { "s" }
        )?;
        for p in &self.problems {
            write!(f, "\n  {}: {}", p.path, p.message)?;
        }
        Ok(())
    }
}

impl ManifestError {
    pub(crate) fn one(file: &Path, path: &str, message: impl Into<String>) -> Self {
        Self {
            file: file.to_path_buf(),
            problems: vec![Problem {
                path: path.into(),
                message: message.into(),
            }],
        }
    }
}

/// Parses + validates + deserializes one YAML manifest from text.
fn parse<T: DeserializeOwned>(
    file: &Path,
    yaml: &str,
    validator: &jsonschema::Validator,
    extra: impl FnOnce(&T) -> Vec<Problem>,
) -> Result<T, ManifestError> {
    let value: Value = serde_yaml_ng::from_str(yaml).map_err(|e| {
        let at = e
            .location()
            .map(|l| format!("line {}, column {}", l.line(), l.column()))
            .unwrap_or_else(|| "/".into());
        ManifestError::one(file, &at, format!("invalid YAML: {e}"))
    })?;
    let problems = schema::validate(validator, &value);
    if !problems.is_empty() {
        return Err(ManifestError {
            file: file.to_path_buf(),
            problems,
        });
    }
    let parsed: T =
        serde_json::from_value(value).map_err(|e| ManifestError::one(file, "/", e.to_string()))?;
    let problems = extra(&parsed);
    if problems.is_empty() {
        Ok(parsed)
    } else {
        Err(ManifestError {
            file: file.to_path_buf(),
            problems,
        })
    }
}

fn problem(path: &str, message: impl Into<String>) -> Problem {
    Problem {
        path: path.into(),
        message: message.into(),
    }
}

/// Bundled schemas, compiled once per process.
static AGENT_VALIDATOR: OnceLock<jsonschema::Validator> = OnceLock::new();
static SKILL_VALIDATOR: OnceLock<jsonschema::Validator> = OnceLock::new();
static PROMPT_VALIDATOR: OnceLock<jsonschema::Validator> = OnceLock::new();

fn compiled(
    cell: &'static OnceLock<jsonschema::Validator>,
    src: &str,
) -> &'static jsonschema::Validator {
    cell.get_or_init(|| {
        schema::compile(&serde_json::from_str(src).expect("bundled schema is valid JSON"))
    })
}

pub fn parse_agent(file: &Path, yaml: &str) -> Result<AgentManifest, ManifestError> {
    parse(
        file,
        yaml,
        compiled(&AGENT_VALIDATOR, AGENT_SCHEMA),
        |a: &AgentManifest| {
            let mut p = Vec::new();
            for (i, s) in a.model_slots.iter().enumerate() {
                if a.model_slots[..i].iter().any(|o| o.slot_id == s.slot_id) {
                    p.push(problem(
                        &format!("/model_slots/{i}/slot_id"),
                        format!("duplicate slot `{}`", s.slot_id),
                    ));
                }
            }
            if a.slot("primary").is_none() {
                p.push(problem("/model_slots", "an agent needs a `primary` slot"));
            }
            p
        },
    )
}

pub fn parse_skill(file: &Path, yaml: &str) -> Result<SkillManifest, ManifestError> {
    parse(
        file,
        yaml,
        compiled(&SKILL_VALIDATOR, SKILL_SCHEMA),
        |s: &SkillManifest| {
            let mut p = Vec::new();
            let need = |field: &str, present: bool, p: &mut Vec<Problem>| {
                if !present {
                    p.push(problem(
                        "/",
                        format!("`kind: {}` needs `{field}`", kind_name(s.kind)),
                    ));
                }
            };
            match s.kind {
                SkillKind::Prompt => need("prompt", s.prompt.is_some(), &mut p),
                SkillKind::ToolChain => need("steps", !s.steps.is_empty(), &mut p),
                SkillKind::Builtin => need("handler", s.handler.is_some(), &mut p),
            }
            for (field, present, allowed) in [
                ("prompt", s.prompt.is_some(), SkillKind::Prompt),
                ("steps", !s.steps.is_empty(), SkillKind::ToolChain),
                ("handler", s.handler.is_some(), SkillKind::Builtin),
            ] {
                if present && s.kind != allowed {
                    p.push(problem(
                        &format!("/{field}"),
                        format!("`{field}` is only used by `kind: {}`", kind_name(allowed)),
                    ));
                }
            }
            for (field, schema) in [
                ("input_schema", Some(&s.input_schema)),
                ("output_schema", s.output_schema.as_ref()),
            ] {
                if let Some(schema) = schema
                    && let Err(e) = jsonschema::meta::validate(schema)
                {
                    p.push(problem(
                        &format!("/{field}{}", e.instance_path()),
                        format!("not a valid JSON Schema: {e}"),
                    ));
                }
            }
            if s.input_schema.get("type").and_then(Value::as_str) != Some("object") {
                p.push(problem(
                    "/input_schema/type",
                    "tool input schemas must have `type: object`",
                ));
            }
            if s.tool_name().len() > 64 {
                p.push(problem(
                    "/id",
                    "id is too long to be a tool name (max 64 characters)",
                ));
            }
            p
        },
    )
}

pub fn parse_prompt_manifest(file: &Path, yaml: &str) -> Result<PromptManifest, ManifestError> {
    parse(
        file,
        yaml,
        compiled(&PROMPT_VALIDATOR, PROMPT_SCHEMA),
        |m: &PromptManifest| {
            let mut p = Vec::new();
            for (i, v) in m.versions.iter().enumerate() {
                if m.versions[..i].iter().any(|o| o.version == v.version) {
                    p.push(problem(
                        &format!("/versions/{i}/version"),
                        format!("duplicate version `{}`", v.version),
                    ));
                }
            }
            if !m.versions.iter().any(|v| v.version == m.active_version) {
                p.push(problem(
                    "/active_version",
                    format!("`{}` is not listed in `versions`", m.active_version),
                ));
            }
            p
        },
    )
}

fn kind_name(k: SkillKind) -> &'static str {
    match k {
        SkillKind::Prompt => "prompt",
        SkillKind::ToolChain => "tool_chain",
        SkillKind::Builtin => "builtin",
    }
}

fn read(file: &Path) -> Result<String, ManifestError> {
    std::fs::read_to_string(file)
        .map_err(|e| ManifestError::one(file, "/", format!("can't read file: {e}")))
}

pub fn load_agent(file: &Path) -> Result<AgentManifest, ManifestError> {
    parse_agent(file, &read(file)?)
}

pub fn load_skill(file: &Path) -> Result<SkillManifest, ManifestError> {
    parse_skill(file, &read(file)?)
}

pub fn load_prompt_manifest(file: &Path) -> Result<PromptManifest, ManifestError> {
    parse_prompt_manifest(file, &read(file)?)
}

/// Every `skill.yaml` under `dir` (recursive), sorted by id. Collects all errors.
pub fn load_skills_dir(dir: &Path) -> Result<Vec<SkillManifest>, Vec<ManifestError>> {
    let mut skills = Vec::new();
    let mut errors = Vec::new();
    for f in find_files(dir, "skill.yaml") {
        match load_skill(&f) {
            Ok(s) => skills.push(s),
            Err(e) => errors.push(e),
        }
    }
    skills.sort_by(|a, b| a.id.cmp(&b.id));
    let mut by_tool: std::collections::HashMap<String, &str> = Default::default();
    for s in &skills {
        match by_tool.insert(s.tool_name(), &s.id) {
            Some(prev) if prev == s.id => errors.push(ManifestError::one(
                dir,
                "/id",
                format!("duplicate skill id `{}`", s.id),
            )),
            // `a.b` and `a__b` both become tool `a__b`.
            Some(prev) => errors.push(ManifestError::one(
                dir,
                "/id",
                format!(
                    "skills `{prev}` and `{}` map to the same tool name `{}`",
                    s.id,
                    s.tool_name()
                ),
            )),
            None => {}
        }
    }
    if errors.is_empty() {
        Ok(skills)
    } else {
        Err(errors)
    }
}

/// Directory levels searched below the root (also bounds pathological trees).
const MAX_DEPTH: usize = 16;

/// Files named `name` under `dir`. Symlinks are never followed (no loops, no escaping
/// the tree), and the walk stops [`MAX_DEPTH`] levels down.
pub(crate) fn find_files(dir: &Path, name: &str) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![(dir.to_path_buf(), 0usize)];
    while let Some((d, depth)) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&d) else {
            continue;
        };
        for e in rd.flatten() {
            // `DirEntry::file_type` does not follow symlinks.
            let Ok(ft) = e.file_type() else { continue };
            if ft.is_dir() && depth < MAX_DEPTH {
                stack.push((e.path(), depth + 1));
            } else if ft.is_file() && e.file_name() == name {
                out.push(e.path());
            }
        }
    }
    out.sort();
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const AGENT: &str = r#"
id: assistant.default
version: 1
description: General assistant
prompt: assistant.system@v1
model_slots:
  - slot_id: primary
    capability: chat
  - slot_id: fast
    capability: chat
    default_model: local:qwen3-1.7b
skills: [text.summarize]
tools: ["mcp.filesystem.*", "artifact.create"]
policies:
  max_steps: 6
  reasoning: auto
"#;

    #[test]
    fn parses_agent() {
        let a = parse_agent(Path::new("agent.yaml"), AGENT).unwrap();
        assert_eq!(a.policies.max_steps, 6);
        assert_eq!(a.policies.caller, "chat.reply");
        assert!(a.slot("primary").unwrap().user_configurable);
        assert_eq!(
            a.slot("fast")
                .unwrap()
                .default_model
                .as_ref()
                .unwrap()
                .to_string(),
            "local:qwen3-1.7b"
        );
        assert!(a.allows_tool("mcp.filesystem.read_file"));
        assert!(a.allows_tool("artifact.create"));
        assert!(!a.allows_tool("artifact.rewrite"));
    }

    #[test]
    fn reports_all_problems_with_paths() {
        let bad = AGENT
            .replace(
                "capability: chat\n  - slot_id: fast",
                "capability: telepathy\n  - slot_id: fast",
            )
            .replace("max_steps: 6", "max_steps: 0\n  colour: blue")
            .replace("local:qwen3-1.7b", "qwen3");
        let e = parse_agent(Path::new("agents/a/agent.yaml"), &bad).unwrap_err();
        let msg = e.to_string();
        assert!(msg.starts_with("agents/a/agent.yaml: 4 problems"), "{msg}");
        assert!(
            msg.contains("/model_slots/0/capability: `telepathy` is not one of `chat`"),
            "{msg}"
        );
        assert!(
            msg.contains("/model_slots/1/default_model: `qwen3` is not a model ref"),
            "{msg}"
        );
        assert!(
            msg.contains("/policies/max_steps: 0 is less than the minimum of 1"),
            "{msg}"
        );
        assert!(
            msg.contains(
                "/policies: Additional properties are not allowed ('colour' was unexpected)"
            ),
            "{msg}"
        );
    }

    #[test]
    fn yaml_errors_have_line_numbers() {
        let e = parse_agent(Path::new("agent.yaml"), "id: [unclosed").unwrap_err();
        assert!(e.problems[0].path.starts_with("line "), "{e}");
    }

    #[test]
    fn skill_kind_specific_fields() {
        let base =
            "id: x.y\nversion: 1\ndescription: d\nrisk: read\ninput_schema: {type: object}\n";
        let e = parse_skill(
            Path::new("s"),
            &format!("{base}kind: prompt\nhandler: x.h\n"),
        )
        .unwrap_err();
        let msg = e.to_string();
        assert!(msg.contains("`kind: prompt` needs `prompt`"), "{msg}");
        assert!(
            msg.contains("/handler: `handler` is only used by `kind: builtin`"),
            "{msg}"
        );
        let s = parse_skill(
            Path::new("s"),
            &format!("{base}kind: builtin\nhandler: clock.now\n"),
        )
        .unwrap();
        assert_eq!(s.tool_spec().name, "x__y");
        let e = parse_skill(
            Path::new("s"),
            "id: x.y\nversion: 1\ndescription: d\nrisk: read\nkind: builtin\nhandler: a.b\ninput_schema: {type: string}\n",
        )
        .unwrap_err();
        assert!(e.to_string().contains("must have `type: object`"));
    }

    #[test]
    fn skill_schemas_must_be_valid_json_schema() {
        let e = parse_skill(
            Path::new("s"),
            "id: x.y\nversion: 1\ndescription: d\nrisk: read\nkind: builtin\nhandler: a.b\ninput_schema: {type: object, properties: {n: {type: integr}}}\n",
        )
        .unwrap_err();
        let msg = e.to_string();
        assert!(
            msg.contains("/input_schema/properties/n/type")
                && msg.contains("not a valid JSON Schema"),
            "{msg}"
        );
    }

    fn tmp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("dn-agent-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn write_skill(dir: &Path, id: &str) {
        let d = dir.join(id);
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(
            d.join("skill.yaml"),
            format!("id: {id}\nversion: 1\ndescription: d\nrisk: read\nkind: builtin\nhandler: a.b\ninput_schema: {{type: object}}\n"),
        )
        .unwrap();
    }

    #[test]
    fn tool_name_collisions_are_rejected() {
        let d = tmp("collide");
        write_skill(&d, "text.sum");
        write_skill(&d, "text__sum");
        let errs = load_skills_dir(&d).unwrap_err();
        assert!(
            errs[0]
                .to_string()
                .contains("map to the same tool name `text__sum`"),
            "{}",
            errs[0]
        );
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn symlink_loops_are_not_followed() {
        let d = tmp("loop");
        write_skill(&d, "a.b");
        std::os::unix::fs::symlink(&d, d.join("a.b").join("loop")).unwrap();
        let skills = load_skills_dir(&d).unwrap();
        assert_eq!(skills.len(), 1);
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn prompt_manifest_active_version_must_exist() {
        let e = parse_prompt_manifest(
            Path::new("m"),
            "id: a.b\nactive_version: v2\nversions:\n  - {version: v1, file: v1.md}\n",
        )
        .unwrap_err();
        assert!(
            e.to_string()
                .contains("/active_version: `v2` is not listed")
        );
    }
}
