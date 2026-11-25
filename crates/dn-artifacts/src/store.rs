//! SQLite store for artifacts (ADR-0004). Versions are append-only: every change, including a
//! rollback, inserts a new version; nothing is ever updated in place.

use crate::error::{ArtifactError, Result};
use dn_core::Id;
use dn_core::artifact::{Artifact, ArtifactOp, ArtifactType, ArtifactVersion, Author};
use rusqlite::{Connection, OptionalExtension, Row, params};
use serde::{Deserialize, Serialize};

/// Schema migrations for this crate, in order. `dn-store` runs them; each entry is one step.
pub fn migrations() -> &'static [&'static str] {
    &[r#"
CREATE TABLE artifact_artifacts (
    id              TEXT PRIMARY KEY NOT NULL,
    conversation_id TEXT NOT NULL,
    -- JSON of dn_core::artifact::ArtifactType, e.g. {"kind":"code","language":"rust"}
    artifact_type   TEXT NOT NULL,
    title           TEXT NOT NULL,
    -- Unused since the fenced text format was removed (ADR-0004 amendment); always NULL.
    -- Kept so existing databases need no migration.
    alias           TEXT,
    created_at      TEXT NOT NULL,
    updated_at      TEXT NOT NULL
);
CREATE INDEX artifact_artifacts_by_conversation
    ON artifact_artifacts (conversation_id, created_at);
CREATE UNIQUE INDEX artifact_artifacts_alias
    ON artifact_artifacts (conversation_id, alias) WHERE alias IS NOT NULL;

CREATE TABLE artifact_versions (
    artifact_id TEXT NOT NULL REFERENCES artifact_artifacts (id) ON DELETE CASCADE,
    n           INTEGER NOT NULL CHECK (n >= 1),
    content     TEXT NOT NULL,
    author      TEXT NOT NULL CHECK (author IN ('model', 'user')),
    parent_n    INTEGER CHECK (parent_n IS NULL OR parent_n < n),
    message_id  TEXT,
    created_at  TEXT NOT NULL,
    PRIMARY KEY (artifact_id, n)
) WITHOUT ROWID;

CREATE TRIGGER artifact_versions_append_only
BEFORE UPDATE ON artifact_versions
BEGIN
    SELECT RAISE(ABORT, 'artifact_versions is append-only');
END;

-- Versions can only go away together with their artifact (ON DELETE CASCADE runs after the
-- parent row is gone, so the WHEN clause lets it through).
CREATE TRIGGER artifact_versions_no_delete
BEFORE DELETE ON artifact_versions
WHEN EXISTS (SELECT 1 FROM artifact_artifacts WHERE id = OLD.artifact_id)
BEGIN
    SELECT RAISE(ABORT, 'artifact_versions is append-only; delete the artifact instead');
END;
"#]
}

/// Largest artifact content, `new_str` or tool argument accepted (bytes).
pub const MAX_CONTENT_BYTES: usize = 2 * 1024 * 1024;
/// Longest title accepted (characters).
pub const MAX_TITLE_CHARS: usize = 200;
/// Artifacts one conversation may hold (a runaway model can't fill the disk).
pub const MAX_ARTIFACTS_PER_CONVERSATION: usize = 100;
/// Versions one artifact may have, including restores.
pub const MAX_VERSIONS_PER_ARTIFACT: u32 = 200;

pub(crate) fn check_content(what: &'static str, s: &str) -> Result<()> {
    if s.len() > MAX_CONTENT_BYTES {
        return Err(ArtifactError::TooLarge {
            what,
            max: MAX_CONTENT_BYTES,
            actual: s.len(),
        });
    }
    Ok(())
}

pub(crate) fn check_title(title: &str) -> Result<()> {
    let n = title.chars().count();
    if n > MAX_TITLE_CHARS {
        return Err(ArtifactError::TitleTooLong {
            max: MAX_TITLE_CHARS,
            actual: n,
        });
    }
    Ok(())
}

/// Artifacts are stored with `\n` line endings, so patches match regardless of how the model
/// or the user's editor wrote them.
pub(crate) fn normalise_newlines(s: &str) -> std::borrow::Cow<'_, str> {
    if s.contains('\r') {
        std::borrow::Cow::Owned(s.replace("\r\n", "\n").replace('\r', "\n"))
    } else {
        std::borrow::Cow::Borrowed(s)
    }
}

/// A write transaction that nests: `BEGIN IMMEDIATE` (takes the write lock up front, so two
/// writers can't both read `MAX(n)`) when the connection is in autocommit mode, otherwise a
/// savepoint inside the caller's transaction. Rolls back on drop unless committed.
struct WriteTx<'c> {
    conn: &'c Connection,
    savepoint: bool,
    done: bool,
}

impl<'c> WriteTx<'c> {
    fn begin(conn: &'c Connection) -> Result<Self> {
        let savepoint = !conn.is_autocommit();
        conn.execute_batch(if savepoint {
            "SAVEPOINT dn_artifact_write"
        } else {
            "BEGIN IMMEDIATE"
        })?;
        Ok(Self {
            conn,
            savepoint,
            done: false,
        })
    }

    /// On failure (e.g. `SQLITE_BUSY` at COMMIT) `done` stays false, so drop rolls back and the
    /// connection is never left inside a half-finished transaction.
    fn commit(mut self) -> Result<()> {
        self.conn.execute_batch(if self.savepoint {
            "RELEASE dn_artifact_write"
        } else {
            "COMMIT"
        })?;
        self.done = true;
        Ok(())
    }
}

impl Drop for WriteTx<'_> {
    fn drop(&mut self) {
        if !self.done {
            let _ = self.conn.execute_batch(if self.savepoint {
                "ROLLBACK TO dn_artifact_write; RELEASE dn_artifact_write"
            } else {
                "ROLLBACK"
            });
        }
    }
}

/// Who made a change and in reply to which message.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OpContext {
    pub author: Author,
    pub message_id: Option<Id>,
}

impl OpContext {
    pub fn model(message_id: Option<Id>) -> Self {
        Self {
            author: Author::Model,
            message_id,
        }
    }

    pub fn user() -> Self {
        Self {
            author: Author::User,
            message_id: None,
        }
    }
}

/// Result of a successful change: the artifact and the version just written.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Applied {
    pub artifact_id: Id,
    pub title: String,
    pub version: ArtifactVersion,
}

/// One row of [`ArtifactStore::list`] (no contents).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtifactSummary {
    pub id: Id,
    pub conversation_id: Id,
    pub artifact_type: ArtifactType,
    pub title: String,
    pub latest_n: u32,
    pub created_at: String,
    pub updated_at: String,
}

/// Artifact store over a connection the app owns. Migrations must have run.
pub struct ArtifactStore<'c> {
    conn: &'c Connection,
}

const NOW: &str = "strftime('%Y-%m-%dT%H:%M:%fZ', 'now')";

impl<'c> ArtifactStore<'c> {
    pub fn new(conn: &'c Connection) -> Self {
        Self { conn }
    }

    /// Apply a model operation inside `conversation_id`. Every write checks that the artifact
    /// belongs to the conversation; another conversation's artifact is [`ArtifactError::NotFound`].
    pub fn apply(&self, conversation_id: Id, op: &ArtifactOp, ctx: &OpContext) -> Result<Applied> {
        match op {
            ArtifactOp::Create {
                artifact_type,
                title,
                content,
            } => self.create(conversation_id, artifact_type, title, content, ctx),
            ArtifactOp::Update {
                id,
                old_str,
                new_str,
            } => self.update(conversation_id, *id, old_str, new_str, ctx),
            ArtifactOp::Rewrite { id, content } => self.rewrite(conversation_id, *id, content, ctx),
        }
    }

    /// Create an artifact with version 1.
    pub fn create(
        &self,
        conversation_id: Id,
        artifact_type: &ArtifactType,
        title: &str,
        content: &str,
        ctx: &OpContext,
    ) -> Result<Applied> {
        check_title(title)?;
        check_content("content", content)?;
        let content = normalise_newlines(content);
        let id = Id::new();
        let type_json = serde_json::to_string(artifact_type)
            .map_err(|e| ArtifactError::Corrupt(e.to_string()))?;
        let tx = WriteTx::begin(self.conn)?;
        let existing: i64 = tx.conn.query_row(
            "SELECT COUNT(*) FROM artifact_artifacts WHERE conversation_id = ?1",
            [conversation_id.to_string()],
            |r| r.get(0),
        )?;
        if existing as usize >= MAX_ARTIFACTS_PER_CONVERSATION {
            return Err(ArtifactError::TooMany {
                what: "artifacts",
                max: MAX_ARTIFACTS_PER_CONVERSATION,
            });
        }
        tx.conn.execute(
            &format!(
                "INSERT INTO artifact_artifacts
                    (id, conversation_id, artifact_type, title, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, {NOW}, {NOW})"
            ),
            params![
                id.to_string(),
                conversation_id.to_string(),
                type_json,
                title
            ],
        )?;
        let version = insert_version(tx.conn, id, 1, &content, None, ctx)?;
        tx.commit()?;
        Ok(Applied {
            artifact_id: id,
            title: title.to_owned(),
            version,
        })
    }

    /// Exact-match patch: `old_str` must occur exactly once in the latest version
    /// (occurrences are counted overlapping, so `"aa"` in `"aaa"` is ambiguous).
    pub fn update(
        &self,
        conversation_id: Id,
        id: Id,
        old_str: &str,
        new_str: &str,
        ctx: &OpContext,
    ) -> Result<Applied> {
        if old_str.is_empty() {
            return Err(ArtifactError::EmptyOldStr);
        }
        check_content("new_str", new_str)?;
        let old_str = normalise_newlines(old_str);
        let new_str = normalise_newlines(new_str);
        let tx = WriteTx::begin(self.conn)?;
        ensure_in(tx.conn, id, conversation_id)?;
        let latest = latest_version(tx.conn, id)?;
        let at = match find_unique(&latest.content, &old_str) {
            Matches::None => return Err(ArtifactError::NoMatch { id, n: latest.n }),
            Matches::Many(count) => {
                return Err(ArtifactError::AmbiguousMatch {
                    id,
                    n: latest.n,
                    count,
                });
            }
            Matches::One(at) => at,
        };
        let mut content =
            String::with_capacity(latest.content.len() - old_str.len() + new_str.len());
        content.push_str(&latest.content[..at]);
        content.push_str(&new_str);
        content.push_str(&latest.content[at + old_str.len()..]);
        check_content("the updated content", &content)?;
        let version = insert_version(tx.conn, id, latest.n + 1, &content, Some(latest.n), ctx)?;
        let title = touch(tx.conn, id)?;
        tx.commit()?;
        Ok(Applied {
            artifact_id: id,
            title,
            version,
        })
    }

    /// Replace the whole content with a new version.
    pub fn rewrite(
        &self,
        conversation_id: Id,
        id: Id,
        content: &str,
        ctx: &OpContext,
    ) -> Result<Applied> {
        check_content("content", content)?;
        let content = normalise_newlines(content);
        let tx = WriteTx::begin(self.conn)?;
        ensure_in(tx.conn, id, conversation_id)?;
        let latest_n = latest_n(tx.conn, id)?;
        let version = insert_version(tx.conn, id, latest_n + 1, &content, Some(latest_n), ctx)?;
        let title = touch(tx.conn, id)?;
        tx.commit()?;
        Ok(Applied {
            artifact_id: id,
            title,
            version,
        })
    }

    /// Restore version `to_n` as a new latest version whose `parent_n` is `to_n`.
    pub fn rollback(
        &self,
        conversation_id: Id,
        id: Id,
        to_n: u32,
        ctx: &OpContext,
    ) -> Result<Applied> {
        let tx = WriteTx::begin(self.conn)?;
        ensure_in(tx.conn, id, conversation_id)?;
        let latest_n = latest_n(tx.conn, id)?;
        let restored = version(tx.conn, id, to_n)?;
        let version = insert_version(
            tx.conn,
            id,
            latest_n + 1,
            &restored.content,
            Some(to_n),
            ctx,
        )?;
        let title = touch(tx.conn, id)?;
        tx.commit()?;
        Ok(Applied {
            artifact_id: id,
            title,
            version,
        })
    }

    /// The artifact with every version, oldest first.
    pub fn get(&self, id: Id) -> Result<Artifact> {
        let (conversation_id, artifact_type, title) = self
            .conn
            .query_row(
                "SELECT conversation_id, artifact_type, title FROM artifact_artifacts WHERE id = ?1",
                [id.to_string()],
                |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, String>(2)?,
                    ))
                },
            )
            .optional()?
            .ok_or(ArtifactError::NotFound { id })?;
        let mut stmt = self.conn.prepare(
            "SELECT n, content, author, parent_n, message_id, created_at
             FROM artifact_versions WHERE artifact_id = ?1 ORDER BY n",
        )?;
        let versions = stmt
            .query_map([id.to_string()], read_version)?
            .collect::<rusqlite::Result<Vec<_>>>()?
            .into_iter()
            .collect::<Result<Vec<_>>>()?;
        Ok(Artifact {
            id,
            conversation_id: parse_id(&conversation_id)?,
            artifact_type: parse_type(&artifact_type)?,
            title,
            versions,
        })
    }

    pub fn version(&self, id: Id, n: u32) -> Result<ArtifactVersion> {
        version(self.conn, id, n)
    }

    pub fn latest(&self, id: Id) -> Result<ArtifactVersion> {
        latest_version(self.conn, id)
    }

    /// Artifacts of a conversation, oldest first.
    pub fn list(&self, conversation_id: Id) -> Result<Vec<ArtifactSummary>> {
        let mut stmt = self.conn.prepare(
            "SELECT a.id, a.artifact_type, a.title, a.created_at, a.updated_at,
                    (SELECT MAX(n) FROM artifact_versions v WHERE v.artifact_id = a.id)
             FROM artifact_artifacts a
             WHERE a.conversation_id = ?1
             ORDER BY a.created_at, a.id",
        )?;
        let rows = stmt
            .query_map([conversation_id.to_string()], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, String>(4)?,
                    r.get::<_, u32>(5)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        rows.into_iter()
            .map(|(id, ty, title, created_at, updated_at, latest_n)| {
                Ok(ArtifactSummary {
                    id: parse_id(&id)?,
                    conversation_id,
                    artifact_type: parse_type(&ty)?,
                    title,
                    latest_n,
                    created_at,
                    updated_at,
                })
            })
            .collect()
    }

    /// Permanently deletes every artifact of `conversation_id` with all its versions (when the
    /// conversation is purged). Returns how many artifacts went.
    pub fn delete_conversation(&self, conversation_id: Id) -> Result<usize> {
        self.purge(Some(conversation_id))
    }

    /// Permanently deletes every artifact and version ("Delete all local data"). Returns how
    /// many artifacts went.
    pub fn delete_all(&self) -> Result<usize> {
        self.purge(None)
    }

    fn purge(&self, conversation_id: Option<Id>) -> Result<usize> {
        let tx = WriteTx::begin(self.conn)?;
        let n = match conversation_id {
            Some(c) => self.conn.execute(
                "DELETE FROM artifact_artifacts WHERE conversation_id = ?1",
                [c.to_string()],
            )?,
            None => self.conn.execute("DELETE FROM artifact_artifacts", [])?,
        };
        // ON DELETE CASCADE removes the versions; this also covers a connection opened
        // without foreign keys (orphaned versions are allowed to go, see the trigger).
        self.conn.execute(
            "DELETE FROM artifact_versions
             WHERE artifact_id NOT IN (SELECT id FROM artifact_artifacts)",
            [],
        )?;
        tx.commit()?;
        Ok(n)
    }

    /// How many artifacts and versions are stored (for the "Delete all local data" summary).
    pub fn counts(&self) -> Result<(u64, u64)> {
        Ok(self.conn.query_row(
            "SELECT (SELECT COUNT(*) FROM artifact_artifacts), (SELECT COUNT(*) FROM artifact_versions)",
            [],
            |r| Ok((r.get::<_, i64>(0)? as u64, r.get::<_, i64>(1)? as u64)),
        )?)
    }

    /// Whether `id` exists and belongs to `conversation_id`.
    pub fn exists_in(&self, id: Id, conversation_id: Id) -> Result<bool> {
        Ok(self
            .conn
            .query_row(
                "SELECT 1 FROM artifact_artifacts WHERE id = ?1 AND conversation_id = ?2",
                params![id.to_string(), conversation_id.to_string()],
                |_| Ok(()),
            )
            .optional()?
            .is_some())
    }
}

fn ensure_in(conn: &Connection, id: Id, conversation_id: Id) -> Result<()> {
    conn.query_row(
        "SELECT 1 FROM artifact_artifacts WHERE id = ?1 AND conversation_id = ?2",
        params![id.to_string(), conversation_id.to_string()],
        |_| Ok(()),
    )
    .optional()?
    .ok_or(ArtifactError::NotFound { id })
}

enum Matches {
    None,
    One(usize),
    Many(usize),
}

/// Count occurrences of `needle` (overlapping) and return the byte offset if there is exactly one.
fn find_unique(hay: &str, needle: &str) -> Matches {
    let mut first = None;
    let mut count = 0;
    let mut start = 0;
    while let Some(i) = hay[start..].find(needle) {
        let at = start + i;
        first.get_or_insert(at);
        count += 1;
        // Advance one char so overlapping occurrences are counted too.
        start = at + hay[at..].chars().next().map_or(1, char::len_utf8);
        if start > hay.len() {
            break;
        }
    }
    match (count, first) {
        (0, _) => Matches::None,
        (1, Some(at)) => Matches::One(at),
        _ => Matches::Many(count),
    }
}

fn insert_version(
    conn: &Connection,
    id: Id,
    n: u32,
    content: &str,
    parent_n: Option<u32>,
    ctx: &OpContext,
) -> Result<ArtifactVersion> {
    if n > MAX_VERSIONS_PER_ARTIFACT {
        return Err(ArtifactError::TooMany {
            what: "versions of one artifact",
            max: MAX_VERSIONS_PER_ARTIFACT as usize,
        });
    }
    let created_at: String = conn.query_row(
        &format!(
            "INSERT INTO artifact_versions
                (artifact_id, n, content, author, parent_n, message_id, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, {NOW})
             RETURNING created_at"
        ),
        params![
            id.to_string(),
            n,
            content,
            author_str(ctx.author),
            parent_n,
            ctx.message_id.map(|m| m.to_string()),
        ],
        |r| r.get(0),
    )?;
    Ok(ArtifactVersion {
        n,
        content: content.to_owned(),
        author: ctx.author,
        parent_n,
        message_id: ctx.message_id,
        created_at,
    })
}

/// Bump `updated_at` and return the title.
fn touch(conn: &Connection, id: Id) -> Result<String> {
    conn.query_row(
        &format!("UPDATE artifact_artifacts SET updated_at = {NOW} WHERE id = ?1 RETURNING title"),
        [id.to_string()],
        |r| r.get(0),
    )
    .optional()?
    .ok_or(ArtifactError::NotFound { id })
}

fn latest_n(conn: &Connection, id: Id) -> Result<u32> {
    conn.query_row(
        "SELECT MAX(n) FROM artifact_versions WHERE artifact_id = ?1",
        [id.to_string()],
        |r| r.get::<_, Option<u32>>(0),
    )?
    .ok_or(ArtifactError::NotFound { id })
}

fn latest_version(conn: &Connection, id: Id) -> Result<ArtifactVersion> {
    conn.query_row(
        "SELECT n, content, author, parent_n, message_id, created_at
         FROM artifact_versions WHERE artifact_id = ?1 ORDER BY n DESC LIMIT 1",
        [id.to_string()],
        read_version,
    )
    .optional()?
    .ok_or(ArtifactError::NotFound { id })?
}

fn version(conn: &Connection, id: Id, n: u32) -> Result<ArtifactVersion> {
    conn.query_row(
        "SELECT n, content, author, parent_n, message_id, created_at
         FROM artifact_versions WHERE artifact_id = ?1 AND n = ?2",
        params![id.to_string(), n],
        read_version,
    )
    .optional()?
    .ok_or(ArtifactError::VersionNotFound { id, n })?
}

/// Row → version. The outer `rusqlite::Result` is for column access, the inner for decoding.
fn read_version(r: &Row<'_>) -> rusqlite::Result<Result<ArtifactVersion>> {
    let n: u32 = r.get(0)?;
    let content: String = r.get(1)?;
    let author: String = r.get(2)?;
    let parent_n: Option<u32> = r.get(3)?;
    let message_id: Option<String> = r.get(4)?;
    let created_at: String = r.get(5)?;
    Ok((|| {
        Ok(ArtifactVersion {
            n,
            content,
            author: parse_author(&author)?,
            parent_n,
            message_id: message_id.as_deref().map(parse_id).transpose()?,
            created_at,
        })
    })())
}

fn author_str(a: Author) -> &'static str {
    match a {
        Author::Model => "model",
        Author::User => "user",
    }
}

fn parse_author(s: &str) -> Result<Author> {
    match s {
        "model" => Ok(Author::Model),
        "user" => Ok(Author::User),
        other => Err(ArtifactError::Corrupt(format!("author `{other}`"))),
    }
}

fn parse_id(s: &str) -> Result<Id> {
    uuid::Uuid::parse_str(s)
        .map(Id)
        .map_err(|e| ArtifactError::Corrupt(format!("id `{s}`: {e}")))
}

fn parse_type(s: &str) -> Result<ArtifactType> {
    serde_json::from_str(s).map_err(|e| ArtifactError::Corrupt(format!("artifact_type `{s}`: {e}")))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) fn db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA foreign_keys = ON;").unwrap();
        for m in migrations() {
            conn.execute_batch(m).unwrap();
        }
        conn
    }

    fn create(store: &ArtifactStore<'_>, conv: Id, content: &str) -> Id {
        store
            .apply(
                conv,
                &ArtifactOp::Create {
                    artifact_type: ArtifactType::Html,
                    title: "Page".into(),
                    content: content.into(),
                },
                &OpContext::model(None),
            )
            .unwrap()
            .artifact_id
    }

    fn update(
        store: &ArtifactStore<'_>,
        conv: Id,
        id: Id,
        old: &str,
        new: &str,
    ) -> Result<Applied> {
        store.apply(
            conv,
            &ArtifactOp::Update {
                id,
                old_str: old.into(),
                new_str: new.into(),
            },
            &OpContext::model(None),
        )
    }

    #[test]
    fn create_writes_v1_without_parent() {
        let conn = db();
        let store = ArtifactStore::new(&conn);
        let conv = Id::new();
        let msg = Id::new();
        let applied = store
            .apply(
                conv,
                &ArtifactOp::Create {
                    artifact_type: ArtifactType::Code {
                        language: "rust".into(),
                    },
                    title: "main.rs".into(),
                    content: "fn main() {}".into(),
                },
                &OpContext::model(Some(msg)),
            )
            .unwrap();
        assert_eq!(applied.version.n, 1);
        assert_eq!(applied.version.parent_n, None);
        let a = store.get(applied.artifact_id).unwrap();
        assert_eq!(a.conversation_id, conv);
        assert_eq!(
            a.artifact_type,
            ArtifactType::Code {
                language: "rust".into()
            }
        );
        assert_eq!(a.versions.len(), 1);
        assert_eq!(a.versions[0].message_id, Some(msg));
        assert_eq!(a.versions[0].author, Author::Model);
        assert!(a.versions[0].created_at.ends_with('Z'));
    }

    #[test]
    fn update_with_one_match_appends_version() {
        let conn = db();
        let store = ArtifactStore::new(&conn);
        let conv = Id::new();
        let id = create(&store, conv, "<h1>Hello</h1><p>world</p>");
        let applied = update(&store, conv, id, "Hello", "Hi").unwrap();
        assert_eq!(applied.version.n, 2);
        assert_eq!(applied.version.parent_n, Some(1));
        assert_eq!(applied.version.content, "<h1>Hi</h1><p>world</p>");
        // v1 is untouched.
        assert_eq!(
            store.version(id, 1).unwrap().content,
            "<h1>Hello</h1><p>world</p>"
        );
        assert_eq!(store.latest(id).unwrap().n, 2);
    }

    #[test]
    fn update_with_zero_matches_fails_and_writes_nothing() {
        let conn = db();
        let store = ArtifactStore::new(&conn);
        let conv = Id::new();
        let id = create(&store, conv, "<h1>Hello</h1>");
        let err = update(&store, conv, id, "hello", "x").unwrap_err();
        assert!(
            matches!(err, ArtifactError::NoMatch { n: 1, .. }),
            "{err:?}"
        );
        assert!(err.to_string().contains("artifact_rewrite"));
        assert_eq!(store.get(id).unwrap().versions.len(), 1);
    }

    #[test]
    fn update_with_many_matches_fails_with_count() {
        let conn = db();
        let store = ArtifactStore::new(&conn);
        let conv = Id::new();
        let id = create(&store, conv, "<li>a</li><li>b</li><li>c</li>");
        let err = update(&store, conv, id, "<li>", "<li class=x>").unwrap_err();
        assert!(
            matches!(err, ArtifactError::AmbiguousMatch { count: 3, n: 1, .. }),
            "{err:?}"
        );
        assert!(err.to_string().contains("3 times"));
        assert_eq!(store.get(id).unwrap().versions.len(), 1);
    }

    #[test]
    fn update_counts_overlapping_matches() {
        let conn = db();
        let store = ArtifactStore::new(&conn);
        let conv = Id::new();
        let id = create(&store, conv, "xaaay");
        let err = update(&store, conv, id, "aa", "b").unwrap_err();
        assert!(matches!(
            err,
            ArtifactError::AmbiguousMatch { count: 2, .. }
        ));
    }

    #[test]
    fn update_handles_multibyte_text() {
        let conn = db();
        let store = ArtifactStore::new(&conn);
        let conv = Id::new();
        let id = create(&store, conv, "سلام 👋 world");
        let v = update(&store, conv, id, "👋", "🌍").unwrap();
        assert_eq!(v.version.content, "سلام 🌍 world");
        assert!(matches!(
            update(&store, conv, id, "", "x").unwrap_err(),
            ArtifactError::EmptyOldStr
        ));
    }

    #[test]
    fn update_applies_against_latest_version() {
        let conn = db();
        let store = ArtifactStore::new(&conn);
        let conv = Id::new();
        let id = create(&store, conv, "one");
        update(&store, conv, id, "one", "two").unwrap();
        // "one" is gone in v2, so patching it again is a no-match against v2.
        let err = update(&store, conv, id, "one", "three").unwrap_err();
        assert!(matches!(err, ArtifactError::NoMatch { n: 2, .. }));
        let v3 = update(&store, conv, id, "two", "three").unwrap();
        assert_eq!((v3.version.n, v3.version.parent_n), (3, Some(2)));
    }

    #[test]
    fn rewrite_replaces_content() {
        let conn = db();
        let store = ArtifactStore::new(&conn);
        let conv = Id::new();
        let id = create(&store, conv, "old");
        let v = store
            .apply(
                conv,
                &ArtifactOp::Rewrite {
                    id,
                    content: "new".into(),
                },
                &OpContext::model(None),
            )
            .unwrap();
        assert_eq!((v.version.n, v.version.parent_n), (2, Some(1)));
        assert_eq!(store.latest(id).unwrap().content, "new");
    }

    #[test]
    fn rollback_is_a_new_version_pointing_at_the_restored_one() {
        let conn = db();
        let store = ArtifactStore::new(&conn);
        let conv = Id::new();
        let id = create(&store, conv, "v1");
        update(&store, conv, id, "v1", "v2").unwrap();
        update(&store, conv, id, "v2", "v3").unwrap();
        let rb = store.rollback(conv, id, 1, &OpContext::user()).unwrap();
        assert_eq!(rb.version.n, 4);
        assert_eq!(rb.version.parent_n, Some(1));
        assert_eq!(rb.version.content, "v1");
        assert_eq!(rb.version.author, Author::User);
        let a = store.get(id).unwrap();
        let contents: Vec<_> = a.versions.iter().map(|v| v.content.as_str()).collect();
        assert_eq!(contents, ["v1", "v2", "v3", "v1"]);
        assert_eq!(a.latest().unwrap().n, 4);
        // Patching after a rollback works on the restored content.
        let v5 = update(&store, conv, id, "v1", "v5").unwrap();
        assert_eq!((v5.version.n, v5.version.parent_n), (5, Some(4)));
    }

    #[test]
    fn rollback_to_missing_version_fails() {
        let conn = db();
        let store = ArtifactStore::new(&conn);
        let conv = Id::new();
        let id = create(&store, conv, "v1");
        let err = store.rollback(conv, id, 7, &OpContext::user()).unwrap_err();
        assert!(matches!(err, ArtifactError::VersionNotFound { n: 7, .. }));
        assert_eq!(store.get(id).unwrap().versions.len(), 1);
    }

    #[test]
    fn versions_are_append_only_in_sql() {
        let conn = db();
        let store = ArtifactStore::new(&conn);
        let conv = Id::new();
        let id = create(&store, conv, "v1");
        let err = conn
            .execute(
                "UPDATE artifact_versions SET content = 'hacked' WHERE artifact_id = ?1",
                [id.to_string()],
            )
            .unwrap_err();
        assert!(err.to_string().contains("append-only"));
    }

    #[test]
    fn ops_on_other_conversations_or_unknown_ids_are_not_found() {
        let conn = db();
        let store = ArtifactStore::new(&conn);
        let (conv_a, conv_b) = (Id::new(), Id::new());
        let id = create(&store, conv_a, "secret");
        let err = update(&store, conv_b, id, "secret", "x").unwrap_err();
        assert!(matches!(err, ArtifactError::NotFound { .. }));
        let err = store
            .apply(
                conv_a,
                &ArtifactOp::Rewrite {
                    id: Id::new(),
                    content: "x".into(),
                },
                &OpContext::model(None),
            )
            .unwrap_err();
        assert!(matches!(err, ArtifactError::NotFound { .. }));
        assert!(matches!(
            store.get(Id::new()).unwrap_err(),
            ArtifactError::NotFound { .. }
        ));
    }

    #[test]
    fn list_returns_conversation_artifacts_in_creation_order() {
        let conn = db();
        let store = ArtifactStore::new(&conn);
        let (conv, other) = (Id::new(), Id::new());
        let a = create(&store, conv, "a");
        let b = create(&store, conv, "b");
        create(&store, other, "c");
        update(&store, conv, b, "b", "bb").unwrap();
        let list = store.list(conv).unwrap();
        let ids: Vec<_> = list.iter().map(|s| s.id).collect();
        assert_eq!(ids, [a, b]);
        assert_eq!(list[1].latest_n, 2);
        assert!(store.list(Id::new()).unwrap().is_empty());
    }

    #[test]
    fn rollback_checks_the_conversation() {
        let conn = db();
        let store = ArtifactStore::new(&conn);
        let (conv, other) = (Id::new(), Id::new());
        let id = create(&store, conv, "v1");
        update(&store, conv, id, "v1", "v2").unwrap();
        let err = store
            .rollback(other, id, 1, &OpContext::user())
            .unwrap_err();
        assert!(matches!(err, ArtifactError::NotFound { .. }));
        assert_eq!(store.get(id).unwrap().versions.len(), 2);
    }

    #[test]
    fn writes_nest_inside_a_callers_transaction() {
        let conn = db();
        let store = ArtifactStore::new(&conn);
        let conv = Id::new();
        conn.execute_batch("BEGIN IMMEDIATE").unwrap();
        let id = create(&store, conv, "v1");
        // A failed write inside the caller's transaction rolls back only itself.
        assert!(update(&store, conv, id, "missing", "x").is_err());
        update(&store, conv, id, "v1", "v2").unwrap();
        assert!(!conn.is_autocommit());
        conn.execute_batch("ROLLBACK").unwrap();
        // The caller rolled back, so nothing was kept.
        assert!(store.list(conv).unwrap().is_empty());
    }

    /// A file database (two connections need one); removed on drop.
    struct TempDb(std::path::PathBuf);
    impl TempDb {
        fn new(journal_mode: &str) -> Self {
            let path = std::env::temp_dir().join(format!("dn-artifacts-test-{}.db", Id::new()));
            let conn = Connection::open(&path).unwrap();
            conn.pragma_update(None, "journal_mode", journal_mode)
                .unwrap();
            for m in migrations() {
                conn.execute_batch(m).unwrap();
            }
            Self(path)
        }
        fn open(&self) -> Connection {
            Connection::open(&self.0).unwrap()
        }
    }
    impl Drop for TempDb {
        fn drop(&mut self) {
            for ext in ["", "-wal", "-shm", "-journal"] {
                let _ = std::fs::remove_file(format!("{}{ext}", self.0.display()));
            }
        }
    }

    #[test]
    fn failed_commit_rolls_back_and_leaves_no_open_transaction() {
        // Rollback-journal mode: COMMIT needs an exclusive lock, so a reader holding a shared
        // lock makes it fail with SQLITE_BUSY.
        let db = TempDb::new("DELETE");
        let writer = db.open();
        writer.busy_timeout(std::time::Duration::ZERO).unwrap();
        let reader = db.open();
        reader
            .execute_batch("BEGIN; SELECT COUNT(*) FROM artifact_artifacts;")
            .unwrap();
        let store = ArtifactStore::new(&writer);
        let conv = Id::new();
        let err = store
            .create(conv, &ArtifactType::Html, "t", "x", &OpContext::user())
            .unwrap_err();
        assert!(err.to_string().contains("locked"), "{err}");
        assert!(
            writer.is_autocommit(),
            "transaction left open after failed COMMIT"
        );
        reader.execute_batch("COMMIT").unwrap();
        assert!(store.list(conv).unwrap().is_empty());
        // The connection is usable again.
        store
            .create(conv, &ArtifactType::Html, "t", "x", &OpContext::user())
            .unwrap();
    }

    #[test]
    fn writes_take_the_write_lock_before_reading() {
        // WAL mode. Another connection holds the write lock and commits a change while our
        // update waits. With BEGIN IMMEDIATE our update waits for the lock and then reads fresh
        // data, so it succeeds. With a DEFERRED transaction it would read a stale snapshot first
        // and fail with SQLITE_BUSY_SNAPSHOT when upgrading to write.
        let db = TempDb::new("WAL");
        let conv = Id::new();
        let (a, b) = {
            let c = db.open();
            let s = ArtifactStore::new(&c);
            (create(&s, conv, "a1"), create(&s, conv, "b1"))
        };
        let other = db.open();
        other.execute_batch("BEGIN IMMEDIATE").unwrap();
        ArtifactStore::new(&other)
            .rewrite(conv, b, "b2", &OpContext::user())
            .unwrap();
        let path = db.0.clone();
        let handle = std::thread::spawn(move || {
            let conn = Connection::open(path).unwrap();
            conn.busy_timeout(std::time::Duration::from_secs(10))
                .unwrap();
            ArtifactStore::new(&conn)
                .update(conv, a, "a1", "a2", &OpContext::user())
                .map(|v| v.version.n)
        });
        std::thread::sleep(std::time::Duration::from_millis(300));
        other.execute_batch("COMMIT").unwrap();
        assert_eq!(handle.join().unwrap().unwrap(), 2);
    }

    #[test]
    fn versions_cannot_be_deleted_but_cascade_with_their_artifact() {
        let conn = db();
        let store = ArtifactStore::new(&conn);
        let conv = Id::new();
        let id = create(&store, conv, "v1");
        let err = conn
            .execute(
                "DELETE FROM artifact_versions WHERE artifact_id = ?1",
                [id.to_string()],
            )
            .unwrap_err();
        assert!(err.to_string().contains("append-only"));
        conn.execute(
            "DELETE FROM artifact_artifacts WHERE id = ?1",
            [id.to_string()],
        )
        .unwrap();
        let left: i64 = conn
            .query_row("SELECT COUNT(*) FROM artifact_versions", [], |r| r.get(0))
            .unwrap();
        assert_eq!(left, 0);
    }

    #[test]
    fn deletes_a_conversation_or_everything() {
        let conn = db();
        let store = ArtifactStore::new(&conn);
        let (a, b) = (Id::new(), Id::new());
        let x = create(&store, a, "x");
        store
            .update(a, x, "x", "y", &OpContext::model(None))
            .unwrap();
        create(&store, a, "z");
        let kept = create(&store, b, "w");
        assert_eq!(store.counts().unwrap(), (3, 4));

        assert_eq!(store.delete_conversation(a).unwrap(), 2);
        assert!(store.list(a).unwrap().is_empty());
        assert!(store.get(kept).is_ok());
        assert_eq!(store.counts().unwrap(), (1, 1));

        assert_eq!(store.delete_all().unwrap(), 1);
        assert_eq!(store.counts().unwrap(), (0, 0));
    }

    #[test]
    fn delete_all_also_clears_versions_without_foreign_keys() {
        let conn = db();
        conn.pragma_update(None, "foreign_keys", false).unwrap();
        let store = ArtifactStore::new(&conn);
        create(&store, Id::new(), "x");
        store.delete_all().unwrap();
        assert_eq!(store.counts().unwrap(), (0, 0));
    }

    #[test]
    fn artifacts_and_versions_are_capped() {
        let conn = db();
        let store = ArtifactStore::new(&conn);
        let conv = Id::new();
        let ctx = OpContext::model(None);
        for _ in 0..MAX_ARTIFACTS_PER_CONVERSATION {
            create(&store, conv, "x");
        }
        let err = store
            .create(conv, &ArtifactType::Html, "one more", "y", &ctx)
            .unwrap_err();
        assert!(
            matches!(
                err,
                ArtifactError::TooMany {
                    what: "artifacts",
                    ..
                }
            ),
            "{err}"
        );
        assert!(
            !err.to_string().contains("artifact_"),
            "no tool names: {err}"
        );
        // Another conversation is unaffected; one artifact's versions are capped too.
        let other = Id::new();
        let id = create(&store, other, "v1");
        for i in 2..=MAX_VERSIONS_PER_ARTIFACT {
            store.rewrite(other, id, &format!("v{i}"), &ctx).unwrap();
        }
        let err = store.rewrite(other, id, "too many", &ctx).unwrap_err();
        assert!(matches!(err, ArtifactError::TooMany { .. }), "{err}");
        let err = store
            .rollback(other, id, 1, &OpContext::user())
            .unwrap_err();
        assert!(matches!(err, ArtifactError::TooMany { .. }), "{err}");
        assert_eq!(store.latest(id).unwrap().n, MAX_VERSIONS_PER_ARTIFACT);
    }

    #[test]
    fn size_limits_are_enforced_with_clear_errors() {
        let conn = db();
        let store = ArtifactStore::new(&conn);
        let conv = Id::new();
        let ctx = OpContext::model(None);
        let big = "x".repeat(MAX_CONTENT_BYTES + 1);
        let err = store
            .create(conv, &ArtifactType::Html, "t", &big, &ctx)
            .unwrap_err();
        assert!(matches!(err, ArtifactError::TooLarge { .. }));
        assert!(err.to_string().contains("limited to"));
        let long_title = "t".repeat(MAX_TITLE_CHARS + 1);
        assert!(matches!(
            store
                .create(conv, &ArtifactType::Html, &long_title, "x", &ctx)
                .unwrap_err(),
            ArtifactError::TitleTooLong { .. }
        ));
        // The patched result is capped too, not only new_str.
        let id = create(
            &store,
            conv,
            &format!("a{}", "y".repeat(MAX_CONTENT_BYTES - 2)),
        );
        let err = update(&store, conv, id, "a", "aaaa").unwrap_err();
        assert!(matches!(err, ArtifactError::TooLarge { .. }));
        assert_eq!(store.get(id).unwrap().versions.len(), 1);
    }

    #[test]
    fn crlf_is_normalised_so_patches_match() {
        let conn = db();
        let store = ArtifactStore::new(&conn);
        let conv = Id::new();
        let id = create(&store, conv, "a\r\nb\r\n");
        assert_eq!(store.latest(id).unwrap().content, "a\nb\n");
        let v = update(&store, conv, id, "a\r\nb", "c\r\nd").unwrap();
        assert_eq!(v.version.content, "c\nd\n");
    }
}

#[cfg(test)]
mod vector_tests {
    use super::tests::db;
    use super::*;

    /// Shared with `ui/artifact-sandbox` so both stores patch the same way.
    #[test]
    fn update_vectors() {
        let v: serde_json::Value =
            serde_json::from_str(include_str!("../tests/vectors/update.json")).unwrap();
        let conn = db();
        let store = ArtifactStore::new(&conn);
        let conv = Id::new();
        for case in v["cases"].as_array().unwrap() {
            let name = case["name"].as_str().unwrap();
            let created = store
                .create(
                    conv,
                    &ArtifactType::Markdown,
                    name,
                    case["content"].as_str().unwrap(),
                    &OpContext::user(),
                )
                .unwrap();
            let got = store.update(
                conv,
                created.artifact_id,
                case["old_str"].as_str().unwrap(),
                case["new_str"].as_str().unwrap(),
                &OpContext::user(),
            );
            let expect = &case["expect"];
            match (expect["ok"].as_str(), got) {
                (Some(want), Ok(a)) => assert_eq!(a.version.content, want, "{name}"),
                (None, Err(e)) => {
                    let code = match e {
                        ArtifactError::NoMatch { .. } => "no_match",
                        ArtifactError::AmbiguousMatch { count, .. } => {
                            assert_eq!(Some(count as u64), expect["count"].as_u64(), "{name}");
                            "ambiguous"
                        }
                        ArtifactError::EmptyOldStr => "empty_old_str",
                        other => panic!("{name}: unexpected {other:?}"),
                    };
                    assert_eq!(Some(code), expect["error"].as_str(), "{name}");
                }
                (_, got) => panic!("{name}: expected {expect}, got {got:?}"),
            }
        }
    }
}
