//! Parity (ADR-0020 §8): answers a golden-prompt set through the agent loop, with mock
//! tools, and records each answer, its tool calls and its run result for the judge.
//!
//! The set's format was agreed with the docs worker (who writes the cases):
//!
//! ```text
//! <set>/prompts/<category>.yaml   a list of cases per category
//! <set>/files/...                 sample files the cases attach (small)
//! <set>/tools.yaml                mock tools: schema, effect (read | write), canned results
//! ```
//!
//! A private holdout (`--holdout`) has the same shape, ids starting `h-`, and must live
//! outside any git work tree, like its report.
//!
//! What a case gets, until the shared runtime (R1) and memory (R3) exist:
//! - a fixed stand-in system prompt (reported as `system: stand-in`), plus the project's
//!   instructions;
//! - project files and attachments, whole, as quoted untrusted excerpts (dn-knowledge's
//!   renderer, the app's);
//! - the case's mock tools: read tools answer from `tools.yaml`, write tools only when the
//!   case `approve`s them, otherwise "The user declined this action.";
//! - no earlier sessions: a memory case is answered without them and reported `memory: none`.
//!
//! `tool_calls.required` / `forbidden` are checked here; `expect.must` / `must_not` are for
//! the judge, which isn't part of R0.

use super::{ModelArgs, Report, Turn, eval_config, in_git_work_tree, turn_with};
use clap::Args;
use dn_agent::{BoxFuture, ToolContext, ToolExecutor, ToolOutput};
use dn_core::CoreError;
use dn_core::chat::{ChatMessage, ContentPart, Role, ToolCall, ToolSpec};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Component, Path, PathBuf};
use std::sync::Mutex;

pub const CATEGORIES: &[&str] = &[
    "chat",
    "writing",
    "summarise",
    "project",
    "tools",
    "multistep",
    "memory",
    "research",
];
pub const LANGS: &[&str] = &["en", "de"];
/// Largest sample file a case may attach.
pub const MAX_FILE_BYTES: u64 = 256 * 1024;
const DEFAULT_MAX_STEPS: u32 = 8;
/// Until R1 moves the app's prompt into the shared runtime.
const STAND_IN_SYSTEM: &str = "You are Demios, a helpful assistant. Answer clearly and \
                               accurately. Use the tools you are given when they help.";
const DECLINED: &str = "The user declined this action.";

#[derive(Args)]
pub struct ParityArgs {
    /// The golden set folder (prompts/, files/, tools.yaml).
    #[arg(long)]
    set: PathBuf,
    /// A private holdout folder with the same shape (outside any git work tree).
    #[arg(long)]
    holdout: Option<PathBuf>,
    #[command(flatten)]
    model: ModelArgs,
    /// Output tokens per model call at most.
    #[arg(long, default_value_t = 1024)]
    max_tokens: u32,
    /// Only cases whose id starts with this.
    #[arg(long)]
    only: Option<String>,
    /// Report path (default: eval-parity-<time>.json; required with --holdout, outside any
    /// git work tree).
    #[arg(long, short)]
    out: Option<PathBuf>,
}

// ---- the files

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Case {
    pub id: String,
    pub category: String,
    pub lang: String,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub project: Option<Project>,
    /// Files attached to the message itself (paths under `files/`).
    #[serde(default)]
    pub attach: Vec<String>,
    /// Mock tools offered (names from `tools.yaml`).
    #[serde(default)]
    pub tools: Vec<String>,
    /// Write tools the user allows in this case; every other write call is declined.
    #[serde(default)]
    pub approve: Vec<String>,
    /// Earlier sessions (memory cases); each alternates user and assistant turns.
    #[serde(default)]
    pub sessions: Vec<Vec<CaseTurn>>,
    /// This session; the last turn is the user's and is the one judged.
    pub turns: Vec<CaseTurn>,
    #[serde(default)]
    pub expect: Expect,
    #[serde(default)]
    pub max_steps: Option<u32>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Project {
    #[serde(default)]
    pub instructions: Option<String>,
    #[serde(default)]
    pub files: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CaseTurn {
    pub role: TurnRole,
    pub content: String,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TurnRole {
    User,
    Assistant,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Expect {
    #[serde(default)]
    pub must: Vec<String>,
    #[serde(default)]
    pub must_not: Vec<String>,
    #[serde(default)]
    pub tool_calls: ToolExpect,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ToolExpect {
    #[serde(default)]
    pub required: Vec<String>,
    #[serde(default)]
    pub forbidden: Vec<String>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MockTool {
    pub name: String,
    pub description: String,
    pub input_schema: serde_json::Value,
    pub effect: Effect,
    #[serde(default)]
    pub results: Vec<MockResult>,
}

impl MockTool {
    /// A read tool named `calculator` with no canned results: the runner does the arithmetic.
    pub fn is_builtin_calculator(&self) -> bool {
        self.name == "calculator" && self.effect == Effect::Read && self.results.is_empty()
    }
}

/// Arithmetic for the built-in calculator: `+ - * /`, parentheses, decimals, unary minus and
/// a postfix `%` (percent: `15%` is 0.15). Nothing else is accepted.
pub fn calculate(expr: &str) -> Result<f64, String> {
    struct P<'a> {
        s: &'a [u8],
        i: usize,
    }
    impl P<'_> {
        fn ws(&mut self) {
            while self.i < self.s.len() && self.s[self.i].is_ascii_whitespace() {
                self.i += 1;
            }
        }
        fn peek(&mut self) -> Option<u8> {
            self.ws();
            self.s.get(self.i).copied()
        }
        fn expr(&mut self, depth: u32) -> Result<f64, String> {
            let mut v = self.term(depth)?;
            while let Some(c @ (b'+' | b'-')) = self.peek() {
                self.i += 1;
                let r = self.term(depth)?;
                v = if c == b'+' { v + r } else { v - r };
            }
            Ok(v)
        }
        fn term(&mut self, depth: u32) -> Result<f64, String> {
            let mut v = self.factor(depth)?;
            while let Some(c @ (b'*' | b'/')) = self.peek() {
                self.i += 1;
                let r = self.factor(depth)?;
                if c == b'/' && r == 0.0 {
                    return Err("division by zero".into());
                }
                v = if c == b'*' { v * r } else { v / r };
            }
            Ok(v)
        }
        fn factor(&mut self, depth: u32) -> Result<f64, String> {
            if depth > 64 {
                return Err("too deeply nested".into());
            }
            let mut v = match self.peek() {
                Some(b'-') => {
                    self.i += 1;
                    -self.factor(depth + 1)?
                }
                Some(b'+') => {
                    self.i += 1;
                    self.factor(depth + 1)?
                }
                Some(b'(') => {
                    self.i += 1;
                    let v = self.expr(depth + 1)?;
                    if self.peek() != Some(b')') {
                        return Err("missing `)`".into());
                    }
                    self.i += 1;
                    v
                }
                Some(c) if c.is_ascii_digit() || c == b'.' => {
                    let start = self.i;
                    while self.i < self.s.len()
                        && (self.s[self.i].is_ascii_digit() || self.s[self.i] == b'.')
                    {
                        self.i += 1;
                    }
                    let t = std::str::from_utf8(&self.s[start..self.i]).unwrap_or_default();
                    t.parse::<f64>()
                        .map_err(|_| format!("`{t}` isn't a number"))?
                }
                Some(c) => return Err(format!("unexpected `{}`", c as char)),
                None => return Err("the expression ends too early".into()),
            };
            while self.peek() == Some(b'%') {
                self.i += 1;
                v /= 100.0;
            }
            Ok(v)
        }
    }
    let mut p = P {
        s: expr.as_bytes(),
        i: 0,
    };
    let v = p.expr(0)?;
    if p.peek().is_some() {
        return Err(format!("unexpected `{}`", p.s[p.i] as char));
    }
    if !v.is_finite() {
        return Err("the result isn't a finite number".into());
    }
    Ok(v)
}

/// A result without float noise: at most 10 decimals, trailing zeros dropped.
pub fn format_number(v: f64) -> String {
    let s = format!("{v:.10}");
    let s = s.trim_end_matches('0').trim_end_matches('.');
    if s == "-0" { "0".into() } else { s.to_string() }
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Effect {
    Read,
    Write,
}

/// `match` (all named arguments contain the value, case-insensitively) or `default`.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MockResult {
    #[serde(default, rename = "match")]
    pub when: Option<BTreeMap<String, String>>,
    #[serde(default)]
    pub result: Option<String>,
    #[serde(default)]
    pub default: Option<String>,
}

/// A set (or a set plus a holdout), read and checked.
#[derive(Debug)]
pub struct Set {
    pub cases: Vec<(Case, SetRoot)>,
    pub tools: HashMap<String, MockTool>,
    /// The set's "today" (`set.yaml`), told to the model in every case.
    pub today: Option<Today>,
}

/// `<set>/set.yaml`.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct SetFile {
    /// ISO date, e.g. `2026-12-07`.
    #[serde(default)]
    today: Option<String>,
}

/// A calendar date, for the "Today is …" line.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Today {
    pub year: i32,
    pub month: u32,
    pub day: u32,
}

impl Today {
    pub fn parse(s: &str) -> Result<Self, String> {
        let bad = || format!("today `{s}` isn't a date like 2026-12-07");
        let mut it = s.trim().splitn(3, '-');
        let (y, m, d) = (it.next(), it.next(), it.next());
        let year: i32 = y.and_then(|v| v.parse().ok()).ok_or_else(bad)?;
        let month: u32 = m.and_then(|v| v.parse().ok()).ok_or_else(bad)?;
        let day: u32 = d.and_then(|v| v.parse().ok()).ok_or_else(bad)?;
        let leap = (year % 4 == 0 && year % 100 != 0) || year % 400 == 0;
        let days = [
            31,
            if leap { 29 } else { 28 },
            31,
            30,
            31,
            30,
            31,
            31,
            30,
            31,
            30,
            31,
        ];
        if !(1..=12).contains(&month) || day == 0 || day > days[month as usize - 1] {
            return Err(bad());
        }
        Ok(Self { year, month, day })
    }

    /// "Monday, 7 December 2026".
    pub fn long(&self) -> String {
        const DAYS: [&str; 7] = [
            "Sunday",
            "Monday",
            "Tuesday",
            "Wednesday",
            "Thursday",
            "Friday",
            "Saturday",
        ];
        const MONTHS: [&str; 12] = [
            "January",
            "February",
            "March",
            "April",
            "May",
            "June",
            "July",
            "August",
            "September",
            "October",
            "November",
            "December",
        ];
        // Sakamoto's method.
        let t = [0, 3, 2, 5, 0, 3, 5, 1, 4, 6, 2, 4];
        let y = if self.month < 3 {
            self.year - 1
        } else {
            self.year
        };
        let w = (y + y / 4 - y / 100 + y / 400 + t[self.month as usize - 1] + self.day as i32)
            .rem_euclid(7);
        format!(
            "{}, {} {} {}",
            DAYS[w as usize],
            self.day,
            MONTHS[self.month as usize - 1],
            self.year
        )
    }
}

/// Which folder a case came from (its `files/` are read from there).
#[derive(Clone, Debug)]
pub struct SetRoot {
    pub dir: PathBuf,
    pub holdout: bool,
}

fn yaml<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<T, String> {
    let s = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    serde_yaml_ng::from_str(&s).map_err(|e| format!("{}: {e}", path.display()))
}

impl Set {
    /// Reads `set` and, if given, `holdout`, and checks every rule.
    pub fn read(set: &Path, holdout: Option<&Path>) -> Result<Self, String> {
        let mut out = Set {
            cases: Vec::new(),
            tools: HashMap::new(),
            today: None,
        };
        out.add(set, false)?;
        if let Some(h) = holdout {
            out.add(h, true)?;
        }
        out.check()?;
        Ok(out)
    }

    fn add(&mut self, dir: &Path, holdout: bool) -> Result<(), String> {
        let set_file = dir.join("set.yaml");
        if set_file.is_file() {
            let f: SetFile = yaml(&set_file)?;
            let today = f
                .today
                .as_deref()
                .map(Today::parse)
                .transpose()
                .map_err(|e| format!("{}: {e}", set_file.display()))?;
            match (holdout, self.today, today) {
                (false, _, t) => self.today = t,
                (true, a, b) if a == b || b.is_none() => {}
                (true, _, _) => {
                    return Err(format!(
                        "{}: `today` differs from the set's",
                        set_file.display()
                    ));
                }
            }
        }
        let tools = dir.join("tools.yaml");
        if tools.is_file() {
            for t in yaml::<Vec<MockTool>>(&tools)? {
                if self.tools.insert(t.name.clone(), t.clone()).is_some() {
                    return Err(format!(
                        "{}: tool `{}` is defined twice",
                        tools.display(),
                        t.name
                    ));
                }
            }
        }
        let prompts = dir.join("prompts");
        let mut files: Vec<PathBuf> = std::fs::read_dir(&prompts)
            .map_err(|e| format!("{}: {e}", prompts.display()))?
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().is_some_and(|x| x == "yaml"))
            .collect();
        files.sort();
        if files.is_empty() {
            return Err(format!("{}: no .yaml files", prompts.display()));
        }
        let root = SetRoot {
            dir: dir.to_path_buf(),
            holdout,
        };
        for f in files {
            for c in yaml::<Vec<Case>>(&f)? {
                self.cases.push((c, root.clone()));
            }
        }
        Ok(())
    }

    fn check(&self) -> Result<(), String> {
        let mut ids = HashSet::new();
        for (c, root) in &self.cases {
            let fail = |why: String| Err(format!("case `{}`: {why}", c.id));
            if c.id.trim().is_empty() {
                return Err("a case has an empty id".into());
            }
            if !ids.insert(c.id.as_str()) {
                return fail("the id appears twice".into());
            }
            if root.holdout != c.id.starts_with("h-") {
                return fail("holdout ids start with `h-`, and only they do".into());
            }
            if !CATEGORIES.contains(&c.category.as_str()) {
                return fail(format!(
                    "category `{}` isn't one of {}",
                    c.category,
                    CATEGORIES.join(", ")
                ));
            }
            if !LANGS.contains(&c.lang.as_str()) {
                return fail(format!("lang `{}` isn't en or de", c.lang));
            }
            match c.turns.last() {
                Some(t) if t.role == TurnRole::User && !t.content.trim().is_empty() => {}
                _ => return fail("the last turn must be a non-empty user turn".into()),
            }
            for s in &c.sessions {
                let alternates = s.iter().enumerate().all(|(i, t)| {
                    t.role
                        == if i % 2 == 0 {
                            TurnRole::User
                        } else {
                            TurnRole::Assistant
                        }
                        && !t.content.trim().is_empty()
                });
                if s.is_empty() || !alternates || s.len() % 2 != 0 {
                    return fail(
                        "each earlier session alternates user and written assistant turns".into(),
                    );
                }
            }
            for name in c
                .tools
                .iter()
                .chain(&c.approve)
                .chain(&c.expect.tool_calls.required)
            {
                if !self.tools.contains_key(name) {
                    return fail(format!("tool `{name}` isn't in tools.yaml"));
                }
            }
            for name in &c.approve {
                if !c.tools.contains(name) {
                    return fail(format!("approves `{name}`, which it doesn't offer"));
                }
            }
            let paths = c.project.iter().flat_map(|p| &p.files).chain(&c.attach);
            for p in paths {
                sample_file(&root.dir, p).map_err(|e| format!("case `{}`: {e}", c.id))?;
            }
        }
        for t in self.tools.values() {
            let n = t.results.len();
            for (i, r) in t.results.iter().enumerate() {
                let ok = match (&r.when, &r.result, &r.default) {
                    (Some(_), Some(_), None) => true,
                    (None, None, Some(_)) => i + 1 == n,
                    _ => false,
                };
                if !ok {
                    return Err(format!(
                        "tool `{}`: each result is `match` + `result`, or a last `default`",
                        t.name
                    ));
                }
            }
        }
        Ok(())
    }
}

/// A sample file's path under `<dir>/files/`, checked: relative, no `..`, at most
/// [`MAX_FILE_BYTES`].
fn sample_file(dir: &Path, rel: &str) -> Result<PathBuf, String> {
    let p = Path::new(rel);
    if !p.components().all(|c| matches!(c, Component::Normal(_))) {
        return Err(format!("file `{rel}` must be a plain path under files/"));
    }
    let full = dir.join("files").join(p);
    let meta = std::fs::metadata(&full).map_err(|e| format!("file `{rel}`: {e}"))?;
    if !meta.is_file() || meta.len() > MAX_FILE_BYTES {
        return Err(format!("file `{rel}` must be a file of at most 256 KB"));
    }
    Ok(full)
}

// ---- running a case

/// One tool call the model made, as the mock answered it.
#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct CallRecord {
    pub name: String,
    pub arguments: serde_json::Value,
    /// The write call was declined (not in the case's `approve`).
    pub declined: bool,
    pub result: String,
}

/// The case's tools, answering from `tools.yaml`.
pub struct MockTools {
    tools: Vec<MockTool>,
    approve: Vec<String>,
    calls: Mutex<Vec<CallRecord>>,
}

impl MockTools {
    pub fn new(set: &Set, case: &Case) -> Self {
        Self {
            tools: case
                .tools
                .iter()
                .filter_map(|n| set.tools.get(n).cloned())
                .collect(),
            approve: case.approve.clone(),
            calls: Mutex::new(Vec::new()),
        }
    }

    pub fn calls(&self) -> Vec<CallRecord> {
        self.calls.lock().map(|c| c.clone()).unwrap_or_default()
    }

    fn answer(&self, call: &ToolCall) -> (bool, String, bool) {
        let Some(t) = self.tools.iter().find(|t| t.name == call.name) else {
            return (
                false,
                format!("There is no tool called `{}`.", call.name),
                true,
            );
        };
        if t.effect == Effect::Write && !self.approve.contains(&t.name) {
            return (true, DECLINED.into(), true);
        }
        if t.is_builtin_calculator() {
            let expr = call.arguments["expression"].as_str().unwrap_or_default();
            return match calculate(expr) {
                Ok(v) => (false, format_number(v), false),
                Err(e) => (false, format!("ERROR: {e}"), true),
            };
        }
        let arg = |k: &str| -> String {
            match &call.arguments[k] {
                serde_json::Value::String(s) => s.to_lowercase(),
                serde_json::Value::Null => String::new(),
                v => v.to_string().to_lowercase(),
            }
        };
        for r in &t.results {
            match (&r.when, &r.result, &r.default) {
                (Some(w), Some(res), _)
                    if w.iter().all(|(k, v)| arg(k).contains(&v.to_lowercase())) =>
                {
                    return (false, res.clone(), false);
                }
                (None, _, Some(d)) => return (false, d.clone(), false),
                _ => {}
            }
        }
        (false, "No result.".into(), false)
    }
}

impl ToolExecutor for MockTools {
    fn tools(&self) -> Vec<ToolSpec> {
        self.tools
            .iter()
            .map(|t| ToolSpec {
                name: t.name.clone(),
                description: t.description.clone(),
                input_schema: t.input_schema.clone(),
            })
            .collect()
    }

    fn execute<'a>(
        &'a self,
        call: &'a ToolCall,
        _ctx: &'a ToolContext,
    ) -> BoxFuture<'a, Result<ToolOutput, CoreError>> {
        let (declined, text, is_error) = self.answer(call);
        if let Ok(mut c) = self.calls.lock() {
            c.push(CallRecord {
                name: call.name.clone(),
                arguments: call.arguments.clone(),
                declined,
                result: text.clone(),
            });
        }
        Box::pin(async move {
            Ok(if is_error {
                ToolOutput::error(text)
            } else {
                ToolOutput::text(text)
            })
        })
    }
}

/// The prompt for a case: system (stand-in + project instructions + files as quoted
/// excerpts) and this session's turns. Earlier sessions aren't included before memory (R3).
pub fn history(
    root: &SetRoot,
    case: &Case,
    today: Option<Today>,
) -> Result<Vec<ChatMessage>, String> {
    let mut system = STAND_IN_SYSTEM.to_string();
    if let Some(t) = today {
        system.push_str(&format!("\n\nToday is {}.", t.long()));
    }
    if let Some(i) = case.project.as_ref().and_then(|p| p.instructions.as_ref()) {
        system.push_str("\n\nProject instructions:\n");
        system.push_str(i);
    }
    let paths = case
        .project
        .iter()
        .flat_map(|p| &p.files)
        .chain(&case.attach);
    let mut hits = Vec::new();
    for (i, rel) in paths.enumerate() {
        let path = sample_file(&root.dir, rel)?;
        let text = std::fs::read_to_string(&path).map_err(|e| format!("{rel}: {e}"))?;
        hits.push(dn_knowledge::Hit {
            chunk_id: i as i64,
            source_id: "case".into(),
            file_id: rel.clone(),
            display_path: rel.clone(),
            start_line: 1,
            end_line: text.lines().count().max(1) as u32,
            text,
            score: 1.0,
        });
    }
    let (block, _) = dn_knowledge::context_block(&hits);
    if !block.is_empty() {
        system.push_str("\n\n");
        system.push_str(&block);
    }
    let mut h = vec![ChatMessage::text(Role::System, system)];
    h.extend(case.turns.iter().map(|t| {
        let role = match t.role {
            TurnRole::User => Role::User,
            TurnRole::Assistant => Role::Assistant,
        };
        ChatMessage {
            role,
            content: vec![ContentPart::Text {
                text: t.content.clone(),
            }],
            tool_calls: vec![],
            tool_call_id: None,
        }
    }));
    Ok(h)
}

#[derive(Debug, Serialize, PartialEq)]
pub struct ToolCheck {
    pub passed: bool,
    pub missing: Vec<String>,
    pub forbidden_called: Vec<String>,
}

/// `tool_calls.required` were all called, and no `forbidden` one was (a declined call
/// counts as called: the model tried).
pub fn check_tools(expect: &ToolExpect, calls: &[CallRecord]) -> ToolCheck {
    let called = |n: &String| calls.iter().any(|c| &c.name == n);
    let missing: Vec<String> = expect
        .required
        .iter()
        .filter(|n| !called(n))
        .cloned()
        .collect();
    let forbidden_called: Vec<String> = expect
        .forbidden
        .iter()
        .filter(|n| called(n))
        .cloned()
        .collect();
    ToolCheck {
        passed: missing.is_empty() && forbidden_called.is_empty(),
        missing,
        forbidden_called,
    }
}

#[derive(Serialize)]
struct Item {
    id: String,
    category: String,
    lang: String,
    tags: Vec<String>,
    /// `none` for a case with earlier sessions: they aren't given before memory exists (R3).
    #[serde(skip_serializing_if = "Option::is_none")]
    memory: Option<&'static str>,
    tool_calls: Vec<CallRecord>,
    tool_check: ToolCheck,
    expect: Expect,
    #[serde(flatten)]
    turn: Turn,
}

pub async fn run(a: ParityArgs) -> Result<(), String> {
    if let Some(h) = &a.holdout {
        if in_git_work_tree(&h.join("prompts")) {
            return Err(format!(
                "the holdout {} is inside a git work tree: keep it outside any repository",
                h.display()
            ));
        }
        match &a.out {
            Some(o) if !in_git_work_tree(o) => {}
            _ => {
                return Err(
                    "with --holdout, the report must go outside any git work tree: pass \
                            --out with a path there"
                        .into(),
                );
            }
        }
    }
    let set = Set::read(&a.set, a.holdout.as_deref())?;
    let cases: Vec<&(Case, SetRoot)> = set
        .cases
        .iter()
        .filter(|(c, _)| a.only.as_ref().is_none_or(|p| c.id.starts_with(p.as_str())))
        .collect();
    let m = a.model.load(None).await?;
    let n = cases.len();
    let mut items = Vec::with_capacity(n);
    for (i, (case, root)) in cases.into_iter().enumerate() {
        let mut cfg = eval_config("eval.parity", &m.model_ref, Some(a.max_tokens));
        cfg.max_steps = case.max_steps.unwrap_or(DEFAULT_MAX_STEPS);
        let tools = MockTools::new(&set, case);
        let t = turn_with(&m.chat, &tools, &cfg, history(root, case, set.today)?).await;
        let calls = tools.calls();
        let check = check_tools(&case.expect.tool_calls, &calls);
        eprintln!(
            "[eval parity] {}/{n} {}: {:?}, {} steps, {} tool calls{}",
            i + 1,
            case.id,
            t.result.stop,
            t.result.steps,
            calls.len(),
            if check.passed {
                ""
            } else {
                ", tool check failed"
            }
        );
        items.push(Item {
            id: case.id.clone(),
            category: case.category.clone(),
            lang: case.lang.clone(),
            tags: case.tags.clone(),
            memory: (!case.sessions.is_empty()).then_some("none"),
            tool_calls: calls,
            tool_check: check,
            expect: case.expect.clone(),
            turn: t,
        });
    }
    let failed = items.iter().filter(|i| i.turn.error.is_some()).count();
    let tool_failed = items.iter().filter(|i| !i.tool_check.passed).count();
    let mut by_category = BTreeMap::<&str, usize>::new();
    for i in &items {
        *by_category.entry(i.category.as_str()).or_default() += 1;
    }
    let summary = serde_json::json!({
        "set": a.set.file_name().map(|s| s.to_string_lossy()),
        "holdout": a.holdout.is_some(),
        "system": "stand-in",
        "cases": items.len(),
        "failed": failed,
        "tool_checks_failed": tool_failed,
        "by_category": by_category,
        "judged": false,
    });
    let report = Report::new("parity", &m, summary, items);
    let path = report.write(a.out)?;
    m.chat.unload().await?;
    println!(
        "{n} cases, {failed} failed, {tool_failed} tool checks failed · report: {}",
        path.display()
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn example() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("eval/parity-example")
    }

    fn call(name: &str, args: serde_json::Value) -> ToolCall {
        ToolCall {
            id: "c".into(),
            name: name.into(),
            arguments: args,
        }
    }

    #[test]
    fn the_example_set_reads_and_renders_files_as_quoted_excerpts() {
        let set = Set::read(&example(), None).unwrap();
        let (case, root) = set
            .cases
            .iter()
            .find(|(c, _)| c.id == "summarise-001")
            .unwrap();
        let h = history(root, case, set.today).unwrap();
        let ContentPart::Text { text: system } = &h[0].content[0] else {
            panic!()
        };
        assert!(system.contains("Answer in British English."));
        assert!(system.contains("untrusted data"));
        assert!(
            system.contains("watering rota"),
            "the minutes are in the excerpts"
        );
        assert_eq!(h.last().unwrap().role, Role::User);
    }

    #[test]
    fn mock_tools_match_arguments_and_decline_unapproved_writes() {
        let set = Set::read(&example(), None).unwrap();
        let (case, _) = set.cases.iter().find(|(c, _)| c.id == "tools-001").unwrap();
        let tools = MockTools::new(&set, case);
        assert_eq!(tools.tools().len(), 2);
        let (_, hit, _) = tools.answer(&call(
            "web_search",
            serde_json::json!({"query": "Heat PUMP costs"}),
        ));
        assert!(hit.contains("heat-pumps"), "{hit}");
        let (_, miss, _) =
            tools.answer(&call("web_search", serde_json::json!({"query": "tulips"})));
        assert_eq!(miss, "No results.");
        let (declined, text, is_error) = tools.answer(&call("send_email", serde_json::json!({})));
        assert!(declined && is_error);
        assert_eq!(text, DECLINED);
    }

    #[test]
    fn tool_checks_count_declined_calls_as_called() {
        let expect = ToolExpect {
            required: vec!["web_search".into()],
            forbidden: vec!["send_email".into()],
        };
        let rec = |n: &str, declined| CallRecord {
            name: n.into(),
            arguments: serde_json::json!({}),
            declined,
            result: String::new(),
        };
        assert!(check_tools(&expect, &[rec("web_search", false)]).passed);
        let c = check_tools(&expect, &[rec("send_email", true)]);
        assert_eq!(
            (c.missing, c.forbidden_called),
            (
                vec!["web_search".to_string()],
                vec!["send_email".to_string()]
            )
        );
    }

    /// Calls `web_search`, then `send_email`, then answers.
    struct Scripted(Mutex<u32>);
    struct Items(std::collections::VecDeque<Result<dn_core::chat::StreamEvent, CoreError>>);
    impl futures_core::Stream for Items {
        type Item = Result<dn_core::chat::StreamEvent, CoreError>;
        fn poll_next(
            mut self: std::pin::Pin<&mut Self>,
            _: &mut std::task::Context<'_>,
        ) -> std::task::Poll<Option<Self::Item>> {
            std::task::Poll::Ready(self.0.pop_front())
        }
    }
    impl dn_core::chat::ChatModel for Scripted {
        fn chat(&self, _: dn_core::chat::ChatRequest) -> dn_core::chat::EventStream {
            use dn_core::chat::{FinishReason, StreamEvent, Usage};
            let mut n = self.0.lock().unwrap();
            *n += 1;
            let done = |f| {
                Ok(StreamEvent::Done {
                    finish_reason: f,
                    usage: Usage {
                        input_tokens: 10,
                        output_tokens: 2,
                        ..Default::default()
                    },
                })
            };
            let evs = match *n {
                1 => vec![
                    Ok(StreamEvent::ToolCall {
                        call: call("web_search", serde_json::json!({"query": "heat pump cost"})),
                    }),
                    done(FinishReason::ToolCalls),
                ],
                2 => vec![
                    Ok(StreamEvent::ToolCall {
                        call: call(
                            "send_email",
                            serde_json::json!({"to": "a@example.com", "body": "x"}),
                        ),
                    }),
                    done(FinishReason::ToolCalls),
                ],
                _ => vec![
                    Ok(StreamEvent::TextDelta {
                        text: "About 8,000–14,000 euros.".into(),
                    }),
                    done(FinishReason::Stop),
                ],
            };
            Box::pin(Items(evs.into()))
        }
    }

    #[tokio::test]
    async fn a_case_runs_through_the_loop_with_mock_tools_and_records_its_calls() {
        let set = Set::read(&example(), None).unwrap();
        let (case, root) = set.cases.iter().find(|(c, _)| c.id == "tools-001").unwrap();
        let model = Scripted(Mutex::new(0));
        let model_ref = dn_core::model::ModelRef::Local {
            model_id: "scripted".into(),
        };
        let mut cfg = eval_config("eval.parity", &model_ref, Some(64));
        cfg.max_steps = DEFAULT_MAX_STEPS;
        let tools = MockTools::new(&set, case);
        let t = turn_with(
            &model,
            &tools,
            &cfg,
            history(root, case, set.today).unwrap(),
        )
        .await;
        assert_eq!(t.text, "About 8,000–14,000 euros.");
        assert_eq!((t.result.steps, t.result.usage.input_tokens), (3, 30));
        let calls = tools.calls();
        assert_eq!(calls.len(), 2);
        assert!(!calls[0].declined && calls[0].result.contains("heat-pumps"));
        assert!(calls[1].declined, "send_email isn't approved in this case");
        let check = check_tools(&case.expect.tool_calls, &calls);
        assert_eq!(check.forbidden_called, ["send_email"]);
        assert!(!check.passed);
    }

    #[test]
    fn the_calculator_does_arithmetic_and_refuses_anything_else() {
        assert_eq!(calculate("2 + 3 * 4").unwrap(), 14.0);
        assert_eq!(calculate("(2 + 3) * 4").unwrap(), 20.0);
        assert_eq!(calculate("-3 + 10 / 4").unwrap(), -0.5);
        assert_eq!(format_number(calculate("80 * 15%").unwrap()), "12");
        assert_eq!(format_number(calculate("0.1 + 0.2").unwrap()), "0.3");
        assert_eq!(format_number(calculate("1 / 3").unwrap()), "0.3333333333");
        assert!(calculate("1 / 0").unwrap_err().contains("zero"));
        assert!(calculate("2 ^ 3").unwrap_err().contains("unexpected"));
        assert!(calculate("sqrt(4)").is_err());
        assert!(calculate("(1 + 2").unwrap_err().contains(")"));
        assert!(calculate("").is_err());
        assert!(calculate(&"(".repeat(200)).unwrap_err().contains("nested"));
    }

    #[test]
    fn today_is_told_in_words_and_a_bad_date_is_refused() {
        let t = Today::parse("2026-12-07").unwrap();
        assert_eq!(t.long(), "Monday, 7 December 2026");
        assert_eq!(
            Today::parse("2024-02-29").unwrap().long(),
            "Thursday, 29 February 2024"
        );
        assert!(Today::parse("2026-02-29").is_err());
        assert!(Today::parse("7.12.2026").is_err());
        let set = Set::read(&example(), None).unwrap();
        assert_eq!(set.today, Some(t));
        let (case, root) = &set.cases[0];
        let h = history(root, case, set.today).unwrap();
        let ContentPart::Text { text } = &h[0].content[0] else {
            panic!()
        };
        assert!(text.contains("Today is Monday, 7 December 2026."));
    }

    #[test]
    fn bad_sets_are_refused_with_the_reason() {
        let dir = std::env::temp_dir().join(format!("dn-parity-{}", dn_core::Id::new()));
        std::fs::create_dir_all(dir.join("prompts")).unwrap();
        std::fs::create_dir_all(dir.join("files")).unwrap();
        let write = |yaml: &str| std::fs::write(dir.join("prompts/chat.yaml"), yaml).unwrap();
        let err = || Set::read(&dir, None).unwrap_err();
        let ok = "- {id: chat-001, category: chat, lang: en, turns: [{role: user, content: hi}]}";
        write(ok);
        assert!(Set::read(&dir, None).is_ok());
        write(&format!("{ok}\n{ok}"));
        assert!(err().contains("appears twice"));
        write("- {id: h-001, category: chat, lang: en, turns: [{role: user, content: hi}]}");
        assert!(err().contains("holdout ids"));
        write("- {id: c, category: poetry, lang: en, turns: [{role: user, content: hi}]}");
        assert!(err().contains("category"));
        write("- {id: c, category: chat, lang: fr, turns: [{role: user, content: hi}]}");
        assert!(err().contains("lang"));
        write(
            "- {id: c, category: chat, lang: en, attach: [../secret.txt], turns: [{role: user, content: hi}]}",
        );
        assert!(err().contains("plain path"));
        write(
            "- {id: c, category: memory, lang: en, sessions: [[{role: user, content: a}]], turns: [{role: user, content: hi}]}",
        );
        assert!(err().contains("alternates"));
        write(
            "- {id: c, category: chat, lang: en, tools: [nope], turns: [{role: user, content: hi}]}",
        );
        assert!(err().contains("isn't in tools.yaml"));
        write(
            "- {id: c, category: chat, lang: en, turns: [{role: user, content: hi}], surprise: 1}",
        );
        assert!(err().contains("unknown field"));
        let _ = std::fs::remove_dir_all(dir);
    }
}
