//! The memory API over `&rusqlite::Connection`.

use rusqlite::{Connection, OptionalExtension, Row, params};
use serde::{Deserialize, Serialize};
use std::fmt;

/// Errors from the memory store.
#[derive(Debug, thiserror::Error)]
pub enum MemoryError {
    #[error("sqlite: {0}")]
    Sqlite(#[from] rusqlite::Error),
    /// Memory is off for the account: nothing is added.
    #[error("memory is off")]
    Off,
    #[error("no memory item {0}")]
    NotFound(String),
    /// Undo after the item's sources were deleted for good.
    #[error("this memory came from something that has since been deleted")]
    SourcesGone,
    /// The call breaks a rule (trust, kind, status); the message says which.
    #[error("{0}")]
    Invalid(String),
}

pub type Result<T> = std::result::Result<T, MemoryError>;

/// Runs `f` as one unit: a savepoint, so it nests inside a caller's transaction or another
/// memory operation, and nothing is left half done when a step fails.
fn atomic<T>(conn: &Connection, f: impl FnOnce() -> Result<T>) -> Result<T> {
    conn.execute_batch("SAVEPOINT memory_op")?;
    match f() {
        Ok(v) => {
            conn.execute_batch("RELEASE memory_op")?;
            Ok(v)
        }
        Err(e) => {
            let _ = conn.execute_batch("ROLLBACK TO memory_op; RELEASE memory_op");
            Err(e)
        }
    }
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}

/// Where an item applies.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind", content = "id")]
pub enum Scope {
    /// Everywhere (the account).
    Account,
    /// One project, by id.
    Project(String),
    /// One add-on's own memory, by add-on id (ADR-0013 §5).
    Addon(String),
}

impl fmt::Display for Scope {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Account => f.write_str("account"),
            Self::Project(id) => write!(f, "project:{id}"),
            Self::Addon(id) => write!(f, "addon:{id}"),
        }
    }
}

impl Scope {
    pub fn parse(s: &str) -> Result<Self> {
        let bad = || MemoryError::Invalid(format!("bad scope {s:?}"));
        if s == "account" {
            return Ok(Self::Account);
        }
        let (k, id) = s.split_once(':').ok_or_else(bad)?;
        if id.is_empty() {
            return Err(bad());
        }
        match k {
            "project" => Ok(Self::Project(id.into())),
            "addon" => Ok(Self::Addon(id.into())),
            _ => Err(bad()),
        }
    }
}

macro_rules! str_enum {
    ($name:ident { $($v:ident = $s:literal),+ $(,)? }) => {
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
        #[serde(rename_all = "snake_case")]
        pub enum $name { $($v),+ }
        impl $name {
            pub fn as_str(self) -> &'static str {
                match self { $(Self::$v => $s),+ }
            }
            pub fn parse(s: &str) -> Result<Self> {
                match s {
                    $($s => Ok(Self::$v),)+
                    _ => Err(MemoryError::Invalid(format!(concat!("bad ", stringify!($name), " {:?}"), s))),
                }
            }
        }
    };
}

str_enum!(Kind {
    Preference = "preference",
    Fact = "fact",
    Person = "person",
    Commitment = "commitment",
    Project = "project",
    Style = "style",
    Correction = "correction",
});

str_enum!(Status {
    Pending = "pending",
    Active = "active",
    Superseded = "superseded",
    Retracted = "retracted",
});

str_enum!(SourceKind {
    Message = "message",
    File = "file",
    Item = "item",
    Addon = "addon",
    Settings = "settings",
});

/// How far an item is trusted (ADR-0020 §4.1), highest last.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Trust {
    ThirdParty = 0,
    OwnContent = 1,
    UserConfirmed = 2,
    UserStated = 3,
}

impl Trust {
    fn from_i64(n: i64) -> Result<Self> {
        Ok(match n {
            0 => Self::ThirdParty,
            1 => Self::OwnContent,
            2 => Self::UserConfirmed,
            3 => Self::UserStated,
            _ => return Err(MemoryError::Invalid(format!("bad trust {n}"))),
        })
    }
}

/// Where an item came from.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Source {
    pub kind: SourceKind,
    /// The message, file or item id.
    pub ref_id: String,
    pub conversation_id: Option<String>,
    /// Shown in Why: the chat title, file name or sender.
    pub title: String,
    pub at: i64,
}

/// One memory item.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Item {
    pub id: String,
    pub scope: Scope,
    pub kind: Kind,
    pub subject: String,
    pub text: String,
    pub status: Status,
    pub trust: Trust,
    /// A claim about its source ("Alice says …"), from someone else's content.
    pub third_party: bool,
    /// `user`, or `model:<model id>@<prompt version>`.
    pub created_by: String,
    pub valid_from: i64,
    pub valid_to: Option<i64>,
    pub superseded_by: Option<String>,
    pub last_used_at: Option<i64>,
    pub last_used_in: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
}

/// An earlier version of an item (Why › Earlier versions).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Version {
    pub text: String,
    pub kind: Kind,
    pub subject: String,
    pub scope: Scope,
    pub valid_from: i64,
    pub valid_to: i64,
}

/// An item with its sources and earlier versions.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Detail {
    pub item: Item,
    pub sources: Vec<Source>,
    pub versions: Vec<Version>,
}

/// A suggestion from the extraction pass (PR 2), or any inferred item.
#[derive(Clone, Debug)]
pub struct Suggestion {
    pub scope: Scope,
    pub kind: Kind,
    pub subject: String,
    pub text: String,
    /// From someone else's content: a claim about the source, trust 0.
    pub third_party: bool,
    /// `model:<model id>@<prompt version>`.
    pub created_by: String,
    pub sources: Vec<Source>,
}

/// The switches for a scope (Settings › Memory and a project's Memory card).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Settings {
    /// Use memory (the account's switch; a project follows the account's).
    pub enabled: bool,
    /// Remember without asking (off by default, the owner's Q2).
    pub remember_automatically: bool,
    /// A project's "Use account memory" (on by default); ignored for other scopes.
    pub use_account: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            enabled: true,
            remember_automatically: false,
            use_account: true,
        }
    }
}

/// What the list shows.
#[derive(Clone, Debug, Default)]
pub struct Filter {
    /// Default: active only.
    pub status: Option<Status>,
    pub kind: Option<Kind>,
    pub subject: Option<String>,
    pub scope: Option<Scope>,
    /// Words to search for in the text and subject.
    pub query: Option<String>,
}

/// A deleted item, held by the app for Undo.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Deleted {
    pub detail: Detail,
}

const COLS: &str = "id, scope, kind, subject, text, status, trust, third_party, created_by, \
                    valid_from, valid_to, superseded_by, last_used_at, last_used_in, created_at, \
                    updated_at";

fn conv<T>(r: Result<T>) -> rusqlite::Result<T> {
    r.map_err(|e| {
        rusqlite::Error::ToSqlConversionFailure(Box::new(std::io::Error::other(e.to_string())))
    })
}

fn item_from(row: &Row<'_>) -> rusqlite::Result<Item> {
    Ok(Item {
        id: row.get(0)?,
        scope: conv(Scope::parse(&row.get::<_, String>(1)?))?,
        kind: conv(Kind::parse(&row.get::<_, String>(2)?))?,
        subject: row.get(3)?,
        text: row.get(4)?,
        status: conv(Status::parse(&row.get::<_, String>(5)?))?,
        trust: conv(Trust::from_i64(row.get(6)?))?,
        third_party: row.get::<_, i64>(7)? == 1,
        created_by: row.get(8)?,
        valid_from: row.get(9)?,
        valid_to: row.get(10)?,
        superseded_by: row.get(11)?,
        last_used_at: row.get(12)?,
        last_used_in: row.get(13)?,
        created_at: row.get(14)?,
        updated_at: row.get(15)?,
    })
}

fn clean(field: &str, s: &str) -> Result<String> {
    let t = s.trim();
    if t.is_empty() {
        return Err(MemoryError::Invalid(format!("{field} is empty")));
    }
    Ok(t.to_owned())
}

fn insert(conn: &Connection, it: &Item, sources: &[Source]) -> Result<()> {
    conn.execute(
        &format!("INSERT INTO memory_items ({COLS}) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16)"),
        params![
            it.id,
            it.scope.to_string(),
            it.kind.as_str(),
            it.subject,
            it.text,
            it.status.as_str(),
            it.trust as i64,
            it.third_party as i64,
            it.created_by,
            it.valid_from,
            it.valid_to,
            it.superseded_by,
            it.last_used_at,
            it.last_used_in,
            it.created_at,
            it.updated_at
        ],
    )?;
    for s in sources {
        conn.execute(
            "INSERT OR IGNORE INTO memory_sources (item_id, kind, ref_id, conversation_id, title, at) \
             VALUES (?1,?2,?3,?4,?5,?6)",
            params![it.id, s.kind.as_str(), s.ref_id, s.conversation_id, s.title, s.at],
        )?;
    }
    index(conn, it)?;
    conn.execute("DELETE FROM memory_tombstones WHERE id = ?1", [&it.id])?;
    Ok(())
}

fn index(conn: &Connection, it: &Item) -> Result<()> {
    conn.execute("DELETE FROM memory_fts WHERE item_id = ?1", [&it.id])?;
    conn.execute(
        "INSERT INTO memory_fts (item_id, subject, text) VALUES (?1, ?2, ?3)",
        params![it.id, it.subject, it.text],
    )?;
    Ok(())
}

fn new_item(scope: Scope, kind: Kind, subject: String, text: String) -> Item {
    let now = now_ms();
    Item {
        id: dn_core::Id::new().to_string(),
        scope,
        kind,
        subject,
        text,
        status: Status::Active,
        trust: Trust::UserStated,
        third_party: false,
        created_by: "user".into(),
        valid_from: now,
        valid_to: None,
        superseded_by: None,
        last_used_at: None,
        last_used_in: None,
        created_at: now,
        updated_at: now,
    }
}

// ---- settings and scopes

pub fn settings(conn: &Connection, scope: &Scope) -> Result<Settings> {
    Ok(conn
        .query_row(
            "SELECT enabled, remember_automatically, use_account FROM memory_settings WHERE scope = ?1",
            [scope.to_string()],
            |r| {
                Ok(Settings {
                    enabled: r.get::<_, i64>(0)? == 1,
                    remember_automatically: r.get::<_, i64>(1)? == 1,
                    use_account: r.get::<_, i64>(2)? == 1,
                })
            },
        )
        .optional()?
        .unwrap_or_default())
}

pub fn set_settings(conn: &Connection, scope: &Scope, s: Settings) -> Result<()> {
    conn.execute(
        "INSERT INTO memory_settings (scope, enabled, remember_automatically, use_account) \
         VALUES (?1,?2,?3,?4) ON CONFLICT(scope) DO UPDATE SET enabled = ?2, \
         remember_automatically = ?3, use_account = ?4",
        params![
            scope.to_string(),
            s.enabled as i64,
            s.remember_automatically as i64,
            s.use_account as i64
        ],
    )?;
    Ok(())
}

/// Whether memory is on (the account's switch).
pub fn enabled(conn: &Connection) -> Result<bool> {
    Ok(settings(conn, &Scope::Account)?.enabled)
}

/// The scopes a chat reads: none with memory off; a project and, with "Use account memory"
/// on, the account; otherwise the account. Incognito chats read nothing (the caller doesn't
/// ask).
pub fn read_scopes(conn: &Connection, project: Option<&str>) -> Result<Vec<Scope>> {
    if !enabled(conn)? {
        return Ok(Vec::new());
    }
    Ok(match project {
        None => vec![Scope::Account],
        Some(p) => {
            let ps = Scope::Project(p.into());
            if settings(conn, &ps)?.use_account {
                vec![ps, Scope::Account]
            } else {
                vec![ps]
            }
        }
    })
}

/// Where a chat's new memories go: its project (decision (c) on #346), else the account.
pub fn write_scope(project: Option<&str>) -> Scope {
    project.map_or(Scope::Account, |p| Scope::Project(p.into()))
}

// ---- adding

/// The person's own memory ("Add memory", or "remember that …" in their own message):
/// active at once, trust 3.
pub fn add_user(
    conn: &Connection,
    scope: Scope,
    kind: Kind,
    subject: &str,
    text: &str,
    source: Option<Source>,
) -> Result<Item> {
    atomic(conn, || {
        if !enabled(conn)? {
            return Err(MemoryError::Off);
        }
        let it = new_item(
            scope,
            kind,
            clean("subject", subject)?,
            clean("text", text)?,
        );
        insert(conn, &it, source.as_slice())?;
        Ok(it)
    })
}

/// An inferred item. It waits as pending, unless it's from the person's own content in a
/// scope with "Remember without asking" on. A third-party item is always a pending claim
/// (trust 0, fact, person or commitment).
pub fn suggest(conn: &Connection, s: Suggestion) -> Result<Item> {
    atomic(conn, || {
        if !enabled(conn)? {
            return Err(MemoryError::Off);
        }
        if s.sources.is_empty() {
            return Err(MemoryError::Invalid(
                "an inferred memory needs its sources".into(),
            ));
        }
        if !s.created_by.starts_with("model:") {
            return Err(MemoryError::Invalid(
                "an inferred memory names the model that suggested it".into(),
            ));
        }
        if s.third_party && !matches!(s.kind, Kind::Fact | Kind::Person | Kind::Commitment) {
            return Err(MemoryError::Invalid(format!(
                "a memory from someone else's content can't be a {}",
                s.kind.as_str()
            )));
        }
        let mut it = new_item(
            s.scope.clone(),
            s.kind,
            clean("subject", &s.subject)?,
            clean("text", &s.text)?,
        );
        it.third_party = s.third_party;
        it.trust = if s.third_party {
            Trust::ThirdParty
        } else {
            Trust::OwnContent
        };
        it.created_by = s.created_by;
        let auto = !s.third_party && settings(conn, &s.scope)?.remember_automatically;
        it.status = if auto {
            Status::Active
        } else {
            Status::Pending
        };
        insert(conn, &it, &s.sources)?;
        Ok(it)
    })
}

// ---- reading

pub fn get(conn: &Connection, id: &str) -> Result<Option<Item>> {
    Ok(conn
        .query_row(
            &format!("SELECT {COLS} FROM memory_items WHERE id = ?1"),
            [id],
            item_from,
        )
        .optional()?)
}

fn must(conn: &Connection, id: &str) -> Result<Item> {
    get(conn, id)?.ok_or_else(|| MemoryError::NotFound(id.into()))
}

pub fn detail(conn: &Connection, id: &str) -> Result<Option<Detail>> {
    let Some(item) = get(conn, id)? else {
        return Ok(None);
    };
    let mut st = conn.prepare(
        "SELECT kind, ref_id, conversation_id, title, at FROM memory_sources WHERE item_id = ?1 \
         ORDER BY at, ref_id",
    )?;
    let sources = st
        .query_map([id], |r| {
            Ok(Source {
                kind: conv(SourceKind::parse(&r.get::<_, String>(0)?))?,
                ref_id: r.get(1)?,
                conversation_id: r.get(2)?,
                title: r.get(3)?,
                at: r.get(4)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut st = conn.prepare(
        "SELECT text, kind, subject, scope, valid_from, valid_to FROM memory_versions \
         WHERE item_id = ?1 ORDER BY valid_to DESC",
    )?;
    let versions = st
        .query_map([id], |r| {
            Ok(Version {
                text: r.get(0)?,
                kind: conv(Kind::parse(&r.get::<_, String>(1)?))?,
                subject: r.get(2)?,
                scope: conv(Scope::parse(&r.get::<_, String>(3)?))?,
                valid_from: r.get(4)?,
                valid_to: r.get(5)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(Some(Detail {
        item,
        sources,
        versions,
    }))
}

/// An FTS5 query matching any of the words in `text` (each quoted), or `None`.
fn fts_words(text: &str) -> Option<String> {
    let words: Vec<String> = text
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .take(16)
        .map(|w| format!("\"{w}\""))
        .collect();
    (!words.is_empty()).then(|| words.join(" OR "))
}

/// The list, newest change first.
pub fn list(conn: &Connection, f: &Filter) -> Result<Vec<Item>> {
    let mut sql = format!("SELECT {COLS} FROM memory_items WHERE status = ?1");
    let mut args: Vec<Box<dyn rusqlite::ToSql>> =
        vec![Box::new(f.status.unwrap_or(Status::Active).as_str())];
    if let Some(k) = f.kind {
        args.push(Box::new(k.as_str()));
        sql.push_str(&format!(" AND kind = ?{}", args.len()));
    }
    if let Some(s) = &f.subject {
        args.push(Box::new(s.clone()));
        sql.push_str(&format!(" AND subject = ?{}", args.len()));
    }
    if let Some(s) = &f.scope {
        args.push(Box::new(s.to_string()));
        sql.push_str(&format!(" AND scope = ?{}", args.len()));
    }
    if let Some(q) = f.query.as_deref() {
        let Some(m) = fts_words(q) else {
            return Ok(Vec::new());
        };
        args.push(Box::new(m));
        sql.push_str(&format!(
            " AND id IN (SELECT item_id FROM memory_fts WHERE memory_fts MATCH ?{})",
            args.len()
        ));
    }
    sql.push_str(" ORDER BY updated_at DESC, id DESC");
    let mut st = conn.prepare(&sql)?;
    let rows = st
        .query_map(rusqlite::params_from_iter(args.iter()), item_from)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

/// The suggestions waiting for the person, newest first, with their sources.
pub fn pending(conn: &Connection) -> Result<Vec<Detail>> {
    let items = list(
        conn,
        &Filter {
            status: Some(Status::Pending),
            ..Default::default()
        },
    )?;
    items
        .iter()
        .map(|i| detail(conn, &i.id)?.ok_or_else(|| MemoryError::NotFound(i.id.clone())))
        .collect()
}

// ---- the pending queue

fn set_status(conn: &Connection, id: &str, from: Status, to: Status) -> Result<Item> {
    let it = must(conn, id)?;
    if it.status != from {
        return Err(MemoryError::Invalid(format!(
            "memory {id} is {}, not {}",
            it.status.as_str(),
            from.as_str()
        )));
    }
    conn.execute(
        "UPDATE memory_items SET status = ?2, updated_at = ?3 WHERE id = ?1",
        params![id, to.as_str(), now_ms()],
    )?;
    must(conn, id)
}

/// Keep a suggestion: active, confirmed by the person (trust 2; a third-party claim stays a
/// claim).
pub fn keep(conn: &Connection, id: &str) -> Result<Item> {
    atomic(conn, || {
        set_status(conn, id, Status::Pending, Status::Active)?;
        conn.execute(
            "UPDATE memory_items SET trust = MAX(trust, 2) WHERE id = ?1",
            [id],
        )?;
        must(conn, id)
    })
}

/// Dismiss a suggestion (retracted, so Undo can bring it back).
pub fn dismiss(conn: &Connection, id: &str) -> Result<Item> {
    set_status(conn, id, Status::Pending, Status::Retracted)
}

/// Undo a dismiss.
pub fn undo_dismiss(conn: &Connection, id: &str) -> Result<Item> {
    set_status(conn, id, Status::Retracted, Status::Pending)
}

/// Dismiss every suggestion; returns their ids for Undo.
pub fn dismiss_all(conn: &Connection) -> Result<Vec<String>> {
    atomic(conn, || {
        let ids: Vec<String> = pending(conn)?.into_iter().map(|d| d.item.id).collect();
        for id in &ids {
            dismiss(conn, id)?;
        }
        Ok(ids)
    })
}

// ---- editing

/// What an edit changes.
#[derive(Clone, Debug)]
pub struct Edit {
    pub text: String,
    pub kind: Kind,
    pub subject: String,
    /// Moving between Everywhere and a project (decision (c) on #346).
    pub scope: Scope,
}

/// Edit an item: the old text becomes an earlier version, and the item is now said by the
/// person (trust 3, no longer a third-party claim). Editing a suggestion keeps it (Edit then
/// Keep in the design).
pub fn edit(conn: &Connection, id: &str, e: Edit) -> Result<Item> {
    atomic(conn, || {
        let old = must(conn, id)?;
        if matches!(old.status, Status::Superseded | Status::Retracted) {
            return Err(MemoryError::Invalid(format!(
                "memory {id} is {}; it can't be edited",
                old.status.as_str()
            )));
        }
        if (matches!(e.scope, Scope::Addon(_)) || matches!(old.scope, Scope::Addon(_)))
            && e.scope != old.scope
        {
            return Err(MemoryError::Invalid(
                "an add-on's memory stays with the add-on".into(),
            ));
        }
        let (text, subject) = (clean("text", &e.text)?, clean("subject", &e.subject)?);
        let now = now_ms();
        conn.execute(
        "INSERT INTO memory_versions (item_id, text, kind, subject, scope, valid_from, valid_to) \
         VALUES (?1,?2,?3,?4,?5,?6,?7)",
        params![
            id,
            old.text,
            old.kind.as_str(),
            old.subject,
            old.scope.to_string(),
            old.valid_from,
            now
        ],
    )?;
        conn.execute(
        "UPDATE memory_items SET text = ?2, kind = ?3, subject = ?4, scope = ?5, status = 'active', \
         trust = 3, third_party = 0, valid_from = ?6, updated_at = ?6 WHERE id = ?1",
        params![id, text, e.kind.as_str(), subject, e.scope.to_string(), now],
    )?;
        // Now the person's: kept when the chat it came from is deleted.
        conn.execute(
            "INSERT OR IGNORE INTO memory_sources (item_id, kind, ref_id, conversation_id, title, at) \
             VALUES (?1, 'settings', ?2, NULL, 'Edited by you', ?3)",
            params![id, format!("edit:{id}"), now],
        )?;
        let it = must(conn, id)?;
        index(conn, &it)?;
        Ok(it)
    })
}

/// Mark `old` superseded by `new` (the supersede pass, PR 2): it keeps its history.
pub fn supersede(conn: &Connection, old: &str, new: &str) -> Result<()> {
    atomic(conn, || {
        if old == new {
            return Err(MemoryError::Invalid(
                "a memory can't supersede itself".into(),
            ));
        }
        let (o, n) = (must(conn, old)?, must(conn, new)?);
        if o.status != Status::Active || n.status != Status::Active {
            return Err(MemoryError::Invalid(
                "only an active memory supersedes another active one".into(),
            ));
        }
        let now = now_ms();
        conn.execute(
            "UPDATE memory_items SET status = 'superseded', valid_to = ?2, superseded_by = ?3, \
         updated_at = ?2 WHERE id = ?1",
            params![old, now, new],
        )?;
        Ok(())
    })
}

// ---- use

/// Record that a message used these items (UsedMemories), and when they were last used.
pub fn record_used(
    conn: &Connection,
    message_id: &str,
    conversation_title: &str,
    used: &[(String, &str)],
) -> Result<()> {
    atomic(conn, || {
        let now = now_ms();
        for (id, how) in used {
            if !matches!(*how, "retrieved" | "profile" | "saved") {
                return Err(MemoryError::Invalid(format!("bad use {how:?}")));
            }
            conn.execute(
                "INSERT OR REPLACE INTO memory_used (message_id, item_id, how) VALUES (?1,?2,?3)",
                params![message_id, id, how],
            )?;
            conn.execute(
                "UPDATE memory_items SET last_used_at = ?2, last_used_in = ?3 WHERE id = ?1",
                params![id, now, conversation_title],
            )?;
        }
        Ok(())
    })
}

/// The items a message used, with how.
pub fn used_by(conn: &Connection, message_id: &str) -> Result<Vec<(Item, String)>> {
    let mut st =
        conn.prepare("SELECT item_id, how FROM memory_used WHERE message_id = ?1 ORDER BY rowid")?;
    let rows: Vec<(String, String)> = st
        .query_map([message_id], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;
    let mut out = Vec::new();
    for (id, how) in rows {
        // An item deleted since isn't shown.
        if let Some(it) = get(conn, &id)? {
            out.push((it, how));
        }
    }
    Ok(out)
}

// ---- deleting

fn remove(conn: &Connection, id: &str) -> Result<()> {
    conn.execute("DELETE FROM memory_fts WHERE item_id = ?1", [id])?;
    conn.execute("DELETE FROM memory_used WHERE item_id = ?1", [id])?;
    conn.execute("DELETE FROM memory_sources WHERE item_id = ?1", [id])?;
    conn.execute("DELETE FROM memory_versions WHERE item_id = ?1", [id])?;
    conn.execute("DELETE FROM memory_items WHERE id = ?1", [id])?;
    conn.execute(
        "INSERT OR REPLACE INTO memory_tombstones (id, deleted_at) VALUES (?1, ?2)",
        params![id, now_ms()],
    )?;
    Ok(())
}

/// Delete an item; the returned copy restores it ([`restore`]) during the Undo window.
pub fn delete(conn: &Connection, id: &str) -> Result<Deleted> {
    atomic(conn, || {
        let detail = detail(conn, id)?.ok_or_else(|| MemoryError::NotFound(id.into()))?;
        remove(conn, id)?;
        Ok(Deleted { detail })
    })
}

/// Undo a delete.
pub fn restore(conn: &Connection, d: &Deleted) -> Result<()> {
    atomic(conn, || {
        let forgotten = |kind: &str, id: &str| -> Result<bool> {
            Ok(conn
                .query_row(
                    "SELECT 1 FROM memory_forgotten WHERE kind = ?1 AND ref_id = ?2",
                    params![kind, id],
                    |_| Ok(()),
                )
                .optional()?
                .is_some())
        };
        let mut sources = Vec::new();
        for s in &d.detail.sources {
            let gone = forgotten(s.kind.as_str(), &s.ref_id)?
                || match &s.conversation_id {
                    Some(c) => forgotten("conversation", c)?,
                    None => false,
                };
            if !gone {
                sources.push(s.clone());
            }
        }
        if !d.detail.sources.is_empty() && sources.is_empty() {
            return Err(MemoryError::SourcesGone);
        }
        insert(conn, &d.detail.item, &sources)?;
        for v in &d.detail.versions {
            conn.execute(
            "INSERT INTO memory_versions (item_id, text, kind, subject, scope, valid_from, valid_to) \
             VALUES (?1,?2,?3,?4,?5,?6,?7)",
            params![
                d.detail.item.id,
                v.text,
                v.kind.as_str(),
                v.subject,
                v.scope.to_string(),
                v.valid_from,
                v.valid_to
            ],
        )?;
        }
        Ok(())
    })
}

/// "Delete all memories": account and project memories and the suggestions waiting; add-on
/// memories stay (each add-on's page deletes its own). Returns copies for Undo.
pub fn delete_all_memories(conn: &Connection) -> Result<Vec<Deleted>> {
    atomic(conn, || {
        let ids: Vec<String> = conn
            .prepare("SELECT id FROM memory_items WHERE scope NOT LIKE 'addon:%'")?
            .query_map([], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        ids.iter().map(|id| delete(conn, id)).collect()
    })
}

/// An add-on's memories (its page's Delete all, or uninstalling it).
pub fn delete_addon(conn: &Connection, addon_id: &str) -> Result<usize> {
    atomic(conn, || {
        let ids: Vec<String> = conn
            .prepare("SELECT id FROM memory_items WHERE scope = ?1")?
            .query_map([Scope::Addon(addon_id.into()).to_string()], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        for id in &ids {
            remove(conn, id)?;
        }
        Ok(ids.len())
    })
}

/// "Delete all local data": every table emptied, settings back to their defaults.
pub fn delete_all_local(conn: &Connection) -> Result<()> {
    atomic(conn, || {
        for t in [
            "memory_fts",
            "memory_used",
            "memory_sources",
            "memory_versions",
            "memory_items",
            "memory_settings",
            "memory_forgotten",
            "memory_tombstones",
        ] {
            conn.execute(&format!("DELETE FROM {t}"), [])?;
        }
        Ok(())
    })
}

/// What's being forgotten (ADR-0013 §5 purge, #280).
#[derive(Clone, Copy, Debug)]
pub enum Forget<'a> {
    /// A whole chat: its messages, as sources.
    Conversation(&'a str),
    /// One source (a message, file, item or add-on record).
    Source(SourceKind, &'a str),
}

/// Forget a source: its rows go, and every item left with no source is deleted (an item the
/// person added in Settings has a `settings` source and is never purged this way). Returns
/// the deleted ids.
pub fn forget(conn: &Connection, what: Forget<'_>) -> Result<Vec<String>> {
    atomic(conn, || {
        let touched: Vec<String> = match what {
            Forget::Conversation(c) => {
                conn.execute(
                        "INSERT OR IGNORE INTO memory_forgotten (kind, ref_id) VALUES ('conversation', ?1)",
                        [c],
                    )?;
                let ids = conn
                    .prepare(
                        "SELECT DISTINCT item_id FROM memory_sources WHERE conversation_id = ?1",
                    )?
                    .query_map([c], |r| r.get(0))?
                    .collect::<rusqlite::Result<_>>()?;
                conn.execute("DELETE FROM memory_sources WHERE conversation_id = ?1", [c])?;
                ids
            }
            Forget::Source(k, r) => {
                conn.execute(
                    "INSERT OR IGNORE INTO memory_forgotten (kind, ref_id) VALUES (?1, ?2)",
                    params![k.as_str(), r],
                )?;
                let ids = conn
                .prepare(
                    "SELECT DISTINCT item_id FROM memory_sources WHERE kind = ?1 AND ref_id = ?2",
                )?
                .query_map(params![k.as_str(), r], |row| row.get(0))?
                .collect::<rusqlite::Result<_>>()?;
                conn.execute(
                    "DELETE FROM memory_sources WHERE kind = ?1 AND ref_id = ?2",
                    params![k.as_str(), r],
                )?;
                ids
            }
        };
        let mut gone = Vec::new();
        for id in touched {
            let left: i64 = conn.query_row(
                "SELECT COUNT(*) FROM memory_sources WHERE item_id = ?1",
                [&id],
                |r| r.get(0),
            )?;
            if left == 0 {
                remove(conn, &id)?;
                gone.push(id);
            }
        }
        Ok(gone)
    })
}

/// Every memory with its sources and earlier versions, for Export (`demios-memories.json`).
pub fn export(conn: &Connection) -> Result<serde_json::Value> {
    let ids: Vec<String> = conn
        .prepare("SELECT id FROM memory_items ORDER BY created_at, id")?
        .query_map([], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    let items = ids
        .iter()
        .map(|id| detail(conn, id)?.ok_or_else(|| MemoryError::NotFound(id.clone())))
        .collect::<Result<Vec<_>>>()?;
    Ok(serde_json::json!({ "format": "demios.memories.v1", "items": items }))
}
