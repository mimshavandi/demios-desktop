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
    -- Model-chosen identifier from the fenced fallback (`<artifact id="todo-app">`), if any.
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
"#]
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

    /// Apply a model operation inside `conversation_id`. Update/Rewrite of an artifact that
    /// belongs to another conversation fails with [`ArtifactError::NotFound`].
    pub fn apply(&self, conversation_id: Id, op: &ArtifactOp, ctx: &OpContext) -> Result<Applied> {
        match op {
            ArtifactOp::Create {
                artifact_type,
                title,
                content,
            } => self.create(conversation_id, artifact_type, title, content, None, ctx),
            ArtifactOp::Update {
                id,
                old_str,
                new_str,
            } => {
                self.ensure_in_conversation(*id, conversation_id)?;
                self.update(*id, old_str, new_str, ctx)
            }
            ArtifactOp::Rewrite { id, content } => {
                self.ensure_in_conversation(*id, conversation_id)?;
                self.rewrite(*id, content, ctx)
            }
        }
    }

    /// Create an artifact with version 1. `alias` is the model's own identifier from the
    /// fenced fallback; it must be unique within the conversation.
    pub fn create(
        &self,
        conversation_id: Id,
        artifact_type: &ArtifactType,
        title: &str,
        content: &str,
        alias: Option<&str>,
        ctx: &OpContext,
    ) -> Result<Applied> {
        let id = Id::new();
        let type_json = serde_json::to_string(artifact_type)
            .map_err(|e| ArtifactError::Corrupt(e.to_string()))?;
        let tx = self.conn.unchecked_transaction()?;
        tx.execute(
            &format!(
                "INSERT INTO artifact_artifacts
                    (id, conversation_id, artifact_type, title, alias, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, {NOW}, {NOW})"
            ),
            params![
                id.to_string(),
                conversation_id.to_string(),
                type_json,
                title,
                alias
            ],
        )?;
        let version = insert_version(&tx, id, 1, content, None, ctx)?;
        tx.commit()?;
        Ok(Applied {
            artifact_id: id,
            title: title.to_owned(),
            version,
        })
    }

    /// Exact-match patch: `old_str` must occur exactly once in the latest version
    /// (occurrences are counted overlapping, so `"aa"` in `"aaa"` is ambiguous).
    pub fn update(&self, id: Id, old_str: &str, new_str: &str, ctx: &OpContext) -> Result<Applied> {
        if old_str.is_empty() {
            return Err(ArtifactError::EmptyOldStr);
        }
        let tx = self.conn.unchecked_transaction()?;
        let latest = latest_version(&tx, id)?;
        let at = match find_unique(&latest.content, old_str) {
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
        content.push_str(new_str);
        content.push_str(&latest.content[at + old_str.len()..]);
        let version = insert_version(&tx, id, latest.n + 1, &content, Some(latest.n), ctx)?;
        let title = touch(&tx, id)?;
        tx.commit()?;
        Ok(Applied {
            artifact_id: id,
            title,
            version,
        })
    }

    /// Replace the whole content with a new version.
    pub fn rewrite(&self, id: Id, content: &str, ctx: &OpContext) -> Result<Applied> {
        let tx = self.conn.unchecked_transaction()?;
        let latest_n = latest_n(&tx, id)?;
        let version = insert_version(&tx, id, latest_n + 1, content, Some(latest_n), ctx)?;
        let title = touch(&tx, id)?;
        tx.commit()?;
        Ok(Applied {
            artifact_id: id,
            title,
            version,
        })
    }

    /// Restore version `to_n` as a new latest version whose `parent_n` is `to_n`.
    pub fn rollback(&self, id: Id, to_n: u32, ctx: &OpContext) -> Result<Applied> {
        let tx = self.conn.unchecked_transaction()?;
        let latest_n = latest_n(&tx, id)?;
        let restored = version(&tx, id, to_n)?;
        let version = insert_version(&tx, id, latest_n + 1, &restored.content, Some(to_n), ctx)?;
        let title = touch(&tx, id)?;
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

    /// Find an artifact of the conversation by its fenced-fallback alias.
    pub fn find_by_alias(&self, conversation_id: Id, alias: &str) -> Result<Option<Id>> {
        self.conn
            .query_row(
                "SELECT id FROM artifact_artifacts WHERE conversation_id = ?1 AND alias = ?2",
                params![conversation_id.to_string(), alias],
                |r| r.get::<_, String>(0),
            )
            .optional()?
            .map(|s| parse_id(&s))
            .transpose()
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

    fn ensure_in_conversation(&self, id: Id, conversation_id: Id) -> Result<()> {
        if self.exists_in(id, conversation_id)? {
            Ok(())
        } else {
            Err(ArtifactError::NotFound { id })
        }
    }
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
        let rb = store.rollback(id, 1, &OpContext::user()).unwrap();
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
        let err = store.rollback(id, 7, &OpContext::user()).unwrap_err();
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
    fn aliases_are_unique_per_conversation() {
        let conn = db();
        let store = ArtifactStore::new(&conn);
        let (conv, other) = (Id::new(), Id::new());
        let ctx = OpContext::model(None);
        let a = store
            .create(
                conv,
                &ArtifactType::Svg,
                "Logo",
                "<svg/>",
                Some("logo"),
                &ctx,
            )
            .unwrap();
        assert!(
            store
                .create(
                    conv,
                    &ArtifactType::Svg,
                    "Logo",
                    "<svg/>",
                    Some("logo"),
                    &ctx
                )
                .is_err()
        );
        store
            .create(
                other,
                &ArtifactType::Svg,
                "Logo",
                "<svg/>",
                Some("logo"),
                &ctx,
            )
            .unwrap();
        assert_eq!(
            store.find_by_alias(conv, "logo").unwrap(),
            Some(a.artifact_id)
        );
        assert_eq!(store.find_by_alias(conv, "nope").unwrap(), None);
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
                    None,
                    &OpContext::user(),
                )
                .unwrap();
            let got = store.update(
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
