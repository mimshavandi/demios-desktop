//! Projects (T-PRJ-01): a project groups chats, attached files and standing instructions
//! that are sent with each of its chats.
//!
//! - Deleting a project is soft, like deleting a chat: the project, its chats and its
//!   files disappear from lists and come back with [`restore_project`] until
//!   [`purge_deleted`] removes them. A purged project or file keeps only a row in
//!   `store_tombstones` (kind `project` / `project_file`), so sync can carry the deletion
//!   (ADR-0008).
//! - A project file is a reference to the file the user picked (`source_path`, shown
//!   only) plus a local copy that the app keeps under `<files root>/<project id>/<file id>`
//!   ([`file_path`]). Files never change: adding a changed file again makes a new row
//!   with a new id, so an index can key on the file id. The store never touches the disk;
//!   the app copies files in and deletes the copies that [`purge_deleted`] reports.

use crate::chat::parse_id;
use crate::{Result, StoreError, now_ms};
use dn_core::Id;
use rusqlite::{Connection, OptionalExtension, Row, params};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// dn-store migration #4 (in the `store` set): projects, project files, a chat's project
/// and the kind of a tombstone.
pub(crate) const MIGRATION: &str = "
CREATE TABLE store_projects (
    id           TEXT    PRIMARY KEY NOT NULL,
    name         TEXT    NOT NULL,
    instructions TEXT    NOT NULL DEFAULT '',
    created_at   INTEGER NOT NULL,
    updated_at   INTEGER NOT NULL,
    deleted_at   INTEGER
);

CREATE TABLE store_project_files (
    id          TEXT    PRIMARY KEY NOT NULL,
    project_id  TEXT    NOT NULL REFERENCES store_projects (id) ON DELETE CASCADE,
    name        TEXT    NOT NULL,
    media_type  TEXT    NOT NULL,
    size        INTEGER NOT NULL,
    sha256      TEXT    NOT NULL,
    source_path TEXT,
    created_at  INTEGER NOT NULL,
    updated_at  INTEGER NOT NULL,
    deleted_at  INTEGER
);
CREATE INDEX store_project_files_project ON store_project_files (project_id, created_at, id);

ALTER TABLE store_conversations ADD COLUMN project_id TEXT
    REFERENCES store_projects (id) ON DELETE SET NULL;
CREATE INDEX store_conversations_project ON store_conversations (project_id);

ALTER TABLE store_tombstones ADD COLUMN kind TEXT NOT NULL DEFAULT 'conversation';
";

/// Longest project name, in characters.
pub const NAME_MAX: usize = 100;
/// Longest standing instructions, in characters.
pub const INSTRUCTIONS_MAX: usize = 8_000;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Project {
    pub id: Id,
    pub name: String,
    /// Standing instructions sent with each chat of the project ("" for none).
    pub instructions: String,
    pub created_at: i64,
    pub updated_at: i64,
}

/// A project with what it holds, for lists.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectSummary {
    #[serde(flatten)]
    pub project: Project,
    pub chats: u64,
    pub files: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectFile {
    pub id: Id,
    pub project_id: Id,
    /// Display name (the picked file's name). Plain text; never a path.
    pub name: String,
    pub media_type: String,
    pub size: u64,
    /// Lowercase hex SHA-256 of the local copy.
    pub sha256: String,
    /// Where the user picked it from. Shown only; never read again.
    pub source_path: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
}

impl ProjectFile {
    /// File name of the local copy inside its project's folder: the file id, never user
    /// text.
    pub fn stored_name(&self) -> String {
        self.id.to_string()
    }
}

/// Where the local copy of `f` lives under `files_root` (the app's `project-files`
/// folder).
pub fn file_path(files_root: &Path, f: &ProjectFile) -> PathBuf {
    project_dir(files_root, f.project_id).join(f.stored_name())
}

/// The folder of a project's local copies under `files_root`.
pub fn project_dir(files_root: &Path, project_id: Id) -> PathBuf {
    files_root.join(project_id.to_string())
}

/// What to write for a file whose copy the app has already made at
/// `file_path(root, ..)` with this `id`.
#[derive(Clone, Debug)]
pub struct NewProjectFile {
    pub id: Id,
    pub name: String,
    pub media_type: String,
    pub size: u64,
    pub sha256: String,
    pub source_path: Option<String>,
}

/// What [`purge_deleted`] removed, so the app can delete the local copies.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Purged {
    pub projects: Vec<Id>,
    /// `(project id, file id)` of every purged file, including those of purged projects.
    pub files: Vec<(Id, Id)>,
}

const PROJECT_COLS: &str = "id, name, instructions, created_at, updated_at";
const FILE_COLS: &str = "f.id, f.project_id, f.name, f.media_type, f.size, f.sha256, f.source_path, f.created_at, f.updated_at";

fn project_from_row(r: &Row<'_>) -> Result<Project> {
    Ok(Project {
        id: parse_id(&r.get::<_, String>(0)?)?,
        name: r.get(1)?,
        instructions: r.get(2)?,
        created_at: r.get(3)?,
        updated_at: r.get(4)?,
    })
}

fn file_from_row(r: &Row<'_>) -> Result<ProjectFile> {
    Ok(ProjectFile {
        id: parse_id(&r.get::<_, String>(0)?)?,
        project_id: parse_id(&r.get::<_, String>(1)?)?,
        name: r.get(2)?,
        media_type: r.get(3)?,
        size: u64::try_from(r.get::<_, i64>(4)?)
            .map_err(|_| StoreError::Invalid("negative file size".into()))?,
        sha256: r.get(5)?,
        source_path: r.get(6)?,
        created_at: r.get(7)?,
        updated_at: r.get(8)?,
    })
}

/// Trims and checks a name and instructions.
fn checked(name: &str, instructions: &str) -> Result<(String, String)> {
    let name = name.trim();
    if name.is_empty() || name.chars().count() > NAME_MAX {
        return Err(StoreError::Invalid(format!(
            "Project name must be 1–{NAME_MAX} characters."
        )));
    }
    let instructions = instructions.trim();
    if instructions.chars().count() > INSTRUCTIONS_MAX {
        return Err(StoreError::Invalid(format!(
            "Instructions must be at most {INSTRUCTIONS_MAX} characters."
        )));
    }
    Ok((name.to_string(), instructions.to_string()))
}

pub fn create_project(conn: &Connection, name: &str) -> Result<Project> {
    let (name, _) = checked(name, "")?;
    let now = now_ms();
    let p = Project {
        id: Id::new(),
        name,
        instructions: String::new(),
        created_at: now,
        updated_at: now,
    };
    conn.execute(
        "INSERT INTO store_projects (id, name, instructions, created_at, updated_at)
         VALUES (?1, ?2, '', ?3, ?3)",
        params![p.id.to_string(), p.name, now],
    )?;
    Ok(p)
}

/// A live (not deleted) project.
pub fn get_project(conn: &Connection, id: Id) -> Result<Project> {
    conn.query_row(
        &format!("SELECT {PROJECT_COLS} FROM store_projects WHERE id = ?1 AND deleted_at IS NULL"),
        [id.to_string()],
        |r| Ok(project_from_row(r)),
    )
    .optional()?
    .ok_or_else(|| StoreError::NotFound(format!("project {id}")))?
}

/// Live projects by name, with how many live chats and files each holds.
pub fn list_projects(conn: &Connection) -> Result<Vec<ProjectSummary>> {
    let mut stmt = conn.prepare(
        "SELECT p.id, p.name, p.instructions, p.created_at, p.updated_at,
            (SELECT COUNT(*) FROM store_conversations c
              WHERE c.project_id = p.id AND c.deleted_at IS NULL),
            (SELECT COUNT(*) FROM store_project_files f
              WHERE f.project_id = p.id AND f.deleted_at IS NULL)
         FROM store_projects p WHERE p.deleted_at IS NULL
         ORDER BY p.name COLLATE NOCASE, p.id",
    )?;
    let mut rows = stmt.query([])?;
    let mut out = vec![];
    while let Some(r) = rows.next()? {
        out.push(ProjectSummary {
            project: project_from_row(r)?,
            chats: r.get::<_, i64>(5)?.try_into().unwrap_or(0),
            files: r.get::<_, i64>(6)?.try_into().unwrap_or(0),
        });
    }
    Ok(out)
}

/// Renames a project and replaces its instructions.
pub fn update_project(
    conn: &Connection,
    id: Id,
    name: &str,
    instructions: &str,
) -> Result<Project> {
    let (name, instructions) = checked(name, instructions)?;
    let n = conn.execute(
        "UPDATE store_projects SET name = ?2, instructions = ?3, updated_at = ?4
         WHERE id = ?1 AND deleted_at IS NULL",
        params![id.to_string(), name, instructions, now_ms()],
    )?;
    if n == 0 {
        return Err(StoreError::NotFound(format!("project {id}")));
    }
    get_project(conn, id)
}

/// Soft-deletes a project and its live chats (with the same deletion time, so
/// [`restore_project`] brings back exactly those). Returns the deleted chats' ids, so the
/// app can stop their answers.
pub fn delete_project(conn: &Connection, id: Id) -> Result<Vec<Id>> {
    let tx = conn.unchecked_transaction()?;
    let now = now_ms();
    let n = tx.execute(
        "UPDATE store_projects SET deleted_at = ?2, updated_at = ?2
         WHERE id = ?1 AND deleted_at IS NULL",
        params![id.to_string(), now],
    )?;
    if n == 0 {
        return Err(StoreError::NotFound(format!("project {id}")));
    }
    let chats = {
        let mut stmt = tx.prepare(
            "SELECT id FROM store_conversations WHERE project_id = ?1 AND deleted_at IS NULL",
        )?;
        let rows = stmt.query_map([id.to_string()], |r| r.get::<_, String>(0))?;
        let mut ids = vec![];
        for row in rows {
            ids.push(parse_id(&row?)?);
        }
        ids
    };
    tx.execute(
        "UPDATE store_conversations SET deleted_at = ?2, updated_at = ?2
         WHERE project_id = ?1 AND deleted_at IS NULL",
        params![id.to_string(), now],
    )?;
    tx.commit()?;
    Ok(chats)
}

/// Undoes [`delete_project`] (the project and the chats it deleted) if not purged yet.
pub fn restore_project(conn: &Connection, id: Id) -> Result<()> {
    let tx = conn.unchecked_transaction()?;
    let deleted_at: Option<i64> = tx
        .query_row(
            "SELECT deleted_at FROM store_projects WHERE id = ?1 AND deleted_at IS NOT NULL",
            [id.to_string()],
            |r| r.get(0),
        )
        .optional()?;
    let deleted_at =
        deleted_at.ok_or_else(|| StoreError::NotFound(format!("deleted project {id}")))?;
    let now = now_ms();
    tx.execute(
        "UPDATE store_projects SET deleted_at = NULL, updated_at = ?2 WHERE id = ?1",
        params![id.to_string(), now],
    )?;
    tx.execute(
        "UPDATE store_conversations SET deleted_at = NULL, updated_at = ?3
         WHERE project_id = ?1 AND deleted_at = ?2",
        params![id.to_string(), deleted_at, now],
    )?;
    tx.commit()?;
    Ok(())
}

/// Records a file whose local copy the app has already written. Fails if the project is
/// not live or already holds `max_files` live files.
pub fn add_file(
    conn: &Connection,
    project_id: Id,
    f: NewProjectFile,
    max_files: u64,
) -> Result<ProjectFile> {
    let tx = conn.unchecked_transaction()?;
    get_project(&tx, project_id)?;
    let count: i64 = tx.query_row(
        "SELECT COUNT(*) FROM store_project_files WHERE project_id = ?1 AND deleted_at IS NULL",
        [project_id.to_string()],
        |r| r.get(0),
    )?;
    if u64::try_from(count).unwrap_or(0) >= max_files {
        return Err(StoreError::Invalid(format!(
            "A project holds at most {max_files} files."
        )));
    }
    let now = now_ms();
    let size = i64::try_from(f.size).map_err(|_| StoreError::Invalid("file too large".into()))?;
    tx.execute(
        "INSERT INTO store_project_files
           (id, project_id, name, media_type, size, sha256, source_path, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?8)",
        params![
            f.id.to_string(),
            project_id.to_string(),
            f.name,
            f.media_type,
            size,
            f.sha256,
            f.source_path,
            now
        ],
    )?;
    tx.execute(
        "UPDATE store_projects SET updated_at = ?2 WHERE id = ?1",
        params![project_id.to_string(), now],
    )?;
    tx.commit()?;
    Ok(ProjectFile {
        id: f.id,
        project_id,
        name: f.name,
        media_type: f.media_type,
        size: f.size,
        sha256: f.sha256,
        source_path: f.source_path,
        created_at: now,
        updated_at: now,
    })
}

/// Live files of a live project, oldest first.
pub fn list_files(conn: &Connection, project_id: Id) -> Result<Vec<ProjectFile>> {
    query_files(conn, "AND f.project_id = ?1", &[&project_id.to_string()])
}

/// Every live file of every live project (for a full reconcile of an index).
pub fn list_all_files(conn: &Connection) -> Result<Vec<ProjectFile>> {
    query_files(conn, "", &[])
}

fn query_files(
    conn: &Connection,
    filter: &str,
    args: &[&dyn rusqlite::ToSql],
) -> Result<Vec<ProjectFile>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {FILE_COLS} FROM store_project_files f
         JOIN store_projects p ON p.id = f.project_id
         WHERE f.deleted_at IS NULL AND p.deleted_at IS NULL {filter}
         ORDER BY f.created_at, f.id"
    ))?;
    let mut rows = stmt.query(args)?;
    let mut out = vec![];
    while let Some(r) = rows.next()? {
        out.push(file_from_row(r)?);
    }
    Ok(out)
}

/// Soft-deletes a file of a live project; [`restore_file`] undoes it until the purge.
pub fn delete_file(conn: &Connection, project_id: Id, file_id: Id) -> Result<()> {
    set_file_deleted(conn, project_id, file_id, true)
}

pub fn restore_file(conn: &Connection, project_id: Id, file_id: Id) -> Result<()> {
    set_file_deleted(conn, project_id, file_id, false)
}

fn set_file_deleted(conn: &Connection, project_id: Id, file_id: Id, delete: bool) -> Result<()> {
    let tx = conn.unchecked_transaction()?;
    get_project(&tx, project_id)?;
    let now = now_ms();
    let (set, cond) = if delete {
        ("deleted_at = ?3", "deleted_at IS NULL")
    } else {
        ("deleted_at = NULL", "deleted_at IS NOT NULL")
    };
    let n = tx.execute(
        &format!(
            "UPDATE store_project_files SET {set}, updated_at = ?3
             WHERE id = ?1 AND project_id = ?2 AND {cond}"
        ),
        params![file_id.to_string(), project_id.to_string(), now],
    )?;
    if n == 0 {
        return Err(StoreError::NotFound(format!("file {file_id}")));
    }
    tx.commit()?;
    Ok(())
}

/// Permanently removes projects and files deleted at or before `before_ms`. Their rows
/// go; only tombstones stay. Chats of a deleted project share its deletion time, so
/// [`crate::chat::purge_deleted`] (run it first) has already purged them.
pub fn purge_deleted(conn: &Connection, before_ms: i64) -> Result<Purged> {
    let tx = conn.unchecked_transaction()?;
    let mut out = Purged::default();
    {
        let mut stmt = tx.prepare(
            "SELECT id FROM store_projects WHERE deleted_at IS NOT NULL AND deleted_at <= ?1",
        )?;
        let rows = stmt.query_map([before_ms], |r| r.get::<_, String>(0))?;
        for row in rows {
            out.projects.push(parse_id(&row?)?);
        }
        let mut stmt = tx.prepare(
            "SELECT f.project_id, f.id FROM store_project_files f
             JOIN store_projects p ON p.id = f.project_id
             WHERE (f.deleted_at IS NOT NULL AND f.deleted_at <= ?1)
                OR (p.deleted_at IS NOT NULL AND p.deleted_at <= ?1)",
        )?;
        let rows = stmt.query_map([before_ms], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })?;
        for row in rows {
            let (p, f) = row?;
            out.files.push((parse_id(&p)?, parse_id(&f)?));
        }
    }
    // A file of a deleted project is gone from the moment the project was deleted.
    tx.execute(
        "INSERT OR REPLACE INTO store_tombstones (id, deleted_at, kind)
         SELECT f.id, MIN(COALESCE(f.deleted_at, p.deleted_at), COALESCE(p.deleted_at, f.deleted_at)),
                'project_file'
         FROM store_project_files f JOIN store_projects p ON p.id = f.project_id
         WHERE (f.deleted_at IS NOT NULL AND f.deleted_at <= ?1)
            OR (p.deleted_at IS NOT NULL AND p.deleted_at <= ?1)",
        [before_ms],
    )?;
    tx.execute(
        "DELETE FROM store_project_files WHERE id IN (
            SELECT f.id FROM store_project_files f
            JOIN store_projects p ON p.id = f.project_id
            WHERE (f.deleted_at IS NOT NULL AND f.deleted_at <= ?1)
               OR (p.deleted_at IS NOT NULL AND p.deleted_at <= ?1))",
        [before_ms],
    )?;
    tx.execute(
        "INSERT OR REPLACE INTO store_tombstones (id, deleted_at, kind)
         SELECT id, deleted_at, 'project' FROM store_projects
         WHERE deleted_at IS NOT NULL AND deleted_at <= ?1",
        [before_ms],
    )?;
    tx.execute(
        "DELETE FROM store_projects WHERE deleted_at IS NOT NULL AND deleted_at <= ?1",
        [before_ms],
    )?;
    tx.commit()?;
    if !out.projects.is_empty() || !out.files.is_empty() {
        crate::checkpoint(conn);
    }
    Ok(out)
}

/// Every project row, live or soft-deleted (not purged): what may own a files folder.
pub fn stored_projects(conn: &Connection) -> Result<Vec<Id>> {
    let mut stmt = conn.prepare("SELECT id FROM store_projects")?;
    let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
    let mut out = vec![];
    for row in rows {
        out.push(parse_id(&row?)?);
    }
    Ok(out)
}

/// `(project id, file id)` of every file row, live or soft-deleted (not purged): which
/// local copies must stay.
pub fn stored_files(conn: &Connection) -> Result<Vec<(Id, Id)>> {
    let mut stmt = conn.prepare("SELECT project_id, id FROM store_project_files")?;
    let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
    let mut out = vec![];
    for row in rows {
        let (p, f) = row?;
        out.push((parse_id(&p)?, parse_id(&f)?));
    }
    Ok(out)
}

/// Ids and deletion times of purged project files, oldest first.
pub fn file_tombstones(conn: &Connection) -> Result<Vec<(Id, i64)>> {
    tombstones(conn, "project_file")
}

/// Ids and deletion times of purged projects, oldest first.
pub fn project_tombstones(conn: &Connection) -> Result<Vec<(Id, i64)>> {
    tombstones(conn, "project")
}

fn tombstones(conn: &Connection, kind: &str) -> Result<Vec<(Id, i64)>> {
    let mut stmt = conn.prepare(
        "SELECT id, deleted_at FROM store_tombstones WHERE kind = ?1 ORDER BY deleted_at, id",
    )?;
    let rows = stmt.query_map([kind], |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))
    })?;
    let mut out = vec![];
    for row in rows {
        let (s, at) = row?;
        out.push((parse_id(&s)?, at));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chat;

    fn db() -> Connection {
        crate::open_in_memory(&[]).unwrap()
    }

    fn new_file(name: &str) -> NewProjectFile {
        NewProjectFile {
            id: Id::new(),
            name: name.into(),
            media_type: "text/plain".into(),
            size: 3,
            sha256: "ab".repeat(32),
            source_path: Some(format!("/home/me/{name}")),
        }
    }

    #[test]
    fn create_list_update_and_counts() {
        let c = db();
        let b = create_project(&c, "  beta ").unwrap();
        assert_eq!(b.name, "beta");
        let a = create_project(&c, "Alpha").unwrap();
        chat::create_conversation_in(&c, "q", Some(a.id)).unwrap();
        chat::create_conversation(&c, "loose").unwrap();
        add_file(&c, a.id, new_file("x.md"), 10).unwrap();

        let list = list_projects(&c).unwrap();
        assert_eq!(
            list.iter()
                .map(|p| p.project.name.as_str())
                .collect::<Vec<_>>(),
            ["Alpha", "beta"],
            "sorted by name, case-insensitively"
        );
        assert_eq!((list[0].chats, list[0].files), (1, 1));
        assert_eq!((list[1].chats, list[1].files), (0, 0));

        let u = update_project(&c, a.id, "Alpha 2", "  Answer in French. ").unwrap();
        assert_eq!(u.instructions, "Answer in French.");
        assert_eq!(get_project(&c, a.id).unwrap().name, "Alpha 2");
        let conv = chat::list_project_conversations(&c, a.id).unwrap();
        assert_eq!(conv.len(), 1);
        assert_eq!(conv[0].project_id, Some(a.id));
    }

    #[test]
    fn names_and_instructions_are_checked() {
        let c = db();
        assert!(matches!(
            create_project(&c, "  "),
            Err(StoreError::Invalid(_))
        ));
        assert!(create_project(&c, &"n".repeat(NAME_MAX)).is_ok());
        assert!(create_project(&c, &"n".repeat(NAME_MAX + 1)).is_err());
        let p = create_project(&c, "p").unwrap();
        assert!(update_project(&c, p.id, "p", &"i".repeat(INSTRUCTIONS_MAX)).is_ok());
        assert!(update_project(&c, p.id, "p", &"i".repeat(INSTRUCTIONS_MAX + 1)).is_err());
    }

    #[test]
    fn a_chat_cannot_join_a_missing_or_deleted_project() {
        let c = db();
        assert!(matches!(
            chat::create_conversation_in(&c, "q", Some(Id::new())),
            Err(StoreError::NotFound(_))
        ));
        let p = create_project(&c, "p").unwrap();
        delete_project(&c, p.id).unwrap();
        assert!(chat::create_conversation_in(&c, "q", Some(p.id)).is_err());
    }

    #[test]
    fn delete_hides_the_project_its_chats_and_files_and_restore_brings_them_back() {
        let c = db();
        let p = create_project(&c, "p").unwrap();
        let in_p = chat::create_conversation_in(&c, "in", Some(p.id)).unwrap();
        let gone_before = chat::create_conversation_in(&c, "gone", Some(p.id)).unwrap();
        let other = chat::create_conversation(&c, "other").unwrap();
        add_file(&c, p.id, new_file("a.txt"), 10).unwrap();
        // A chat deleted on its own before the project stays deleted after restore.
        chat::delete_conversation(&c, gone_before.id).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(2));

        assert_eq!(delete_project(&c, p.id).unwrap(), vec![in_p.id]);
        assert!(list_projects(&c).unwrap().is_empty());
        assert!(list_all_files(&c).unwrap().is_empty());
        let live: Vec<_> = chat::list_conversations(&c)
            .unwrap()
            .into_iter()
            .map(|x| x.id)
            .collect();
        assert_eq!(live, vec![other.id]);
        assert!(get_project(&c, p.id).is_err());
        assert!(update_project(&c, p.id, "x", "").is_err());

        restore_project(&c, p.id).unwrap();
        assert_eq!(list_projects(&c).unwrap().len(), 1);
        assert_eq!(list_files(&c, p.id).unwrap().len(), 1);
        let ids: Vec<_> = chat::list_project_conversations(&c, p.id)
            .unwrap()
            .into_iter()
            .map(|x| x.id)
            .collect();
        assert_eq!(ids, vec![in_p.id]);
        assert!(
            restore_project(&c, p.id).is_err(),
            "only a deleted project can be restored"
        );
    }

    #[test]
    fn files_are_capped_scoped_and_soft_deleted() {
        let c = db();
        let p = create_project(&c, "p").unwrap();
        let q = create_project(&c, "q").unwrap();
        let a = add_file(&c, p.id, new_file("a.txt"), 2).unwrap();
        add_file(&c, p.id, new_file("b.txt"), 2).unwrap();
        assert!(matches!(
            add_file(&c, p.id, new_file("c.txt"), 2),
            Err(StoreError::Invalid(_))
        ));
        assert!(
            matches!(delete_file(&c, q.id, a.id), Err(StoreError::NotFound(_))),
            "a file is only reachable through its own project"
        );

        delete_file(&c, p.id, a.id).unwrap();
        assert_eq!(list_files(&c, p.id).unwrap().len(), 1);
        add_file(&c, p.id, new_file("c.txt"), 2).unwrap();
        assert!(restore_file(&c, p.id, a.id).is_ok());
        assert_eq!(list_files(&c, p.id).unwrap().len(), 3);
        assert_eq!(list_all_files(&c).unwrap()[0].name, "a.txt");
        let f = &list_files(&c, p.id).unwrap()[0];
        assert_eq!(
            file_path(Path::new("/root"), f),
            Path::new("/root")
                .join(p.id.to_string())
                .join(a.id.to_string())
        );
    }

    #[test]
    fn purge_keeps_only_tombstones_and_reports_what_to_delete_on_disk() {
        let c = db();
        let p = create_project(&c, "secret project").unwrap();
        update_project(&c, p.id, "secret project", "secret instructions").unwrap();
        let conv = chat::create_conversation_in(&c, "in", Some(p.id)).unwrap();
        chat::add_message(&c, conv.id, chat::NewMessage::user_text("hello")).unwrap();
        let f1 = add_file(&c, p.id, new_file("one.txt"), 10).unwrap();
        let keep = create_project(&c, "keep").unwrap();
        let f2 = add_file(&c, keep.id, new_file("two.txt"), 10).unwrap();
        let f3 = add_file(&c, keep.id, new_file("three.txt"), 10).unwrap();
        delete_file(&c, keep.id, f2.id).unwrap();
        delete_project(&c, p.id).unwrap();

        let now = now_ms();
        assert_eq!(chat::purge_deleted(&c, now).unwrap(), vec![conv.id]);
        let purged = purge_deleted(&c, now).unwrap();
        assert_eq!(purged.projects, vec![p.id]);
        let mut files = purged.files.clone();
        files.sort();
        let mut want = vec![(p.id, f1.id), (keep.id, f2.id)];
        want.sort();
        assert_eq!(files, want);

        let rows: i64 = c
            .query_row("SELECT COUNT(*) FROM store_projects", [], |r| r.get(0))
            .unwrap();
        assert_eq!(rows, 1);
        let files: Vec<_> = list_all_files(&c)
            .unwrap()
            .into_iter()
            .map(|f| f.id)
            .collect();
        assert_eq!(files, vec![f3.id]);
        assert_eq!(
            project_tombstones(&c)
                .unwrap()
                .iter()
                .map(|t| t.0)
                .collect::<Vec<_>>(),
            vec![p.id]
        );
        let mut ft: Vec<_> = file_tombstones(&c)
            .unwrap()
            .into_iter()
            .map(|t| t.0)
            .collect();
        ft.sort();
        let mut want = vec![f1.id, f2.id];
        want.sort();
        assert_eq!(ft, want);
        // Chat tombstones stay chat-only (other crates forget chats by these ids).
        assert_eq!(
            chat::list_tombstones(&c)
                .unwrap()
                .iter()
                .map(|t| t.0)
                .collect::<Vec<_>>(),
            vec![conv.id]
        );
        assert!(chat::is_purged(&c, &conv.id.to_string()).unwrap());
        assert!(!chat::is_purged(&c, &p.id.to_string()).unwrap());
        // Nothing to do a second time.
        assert_eq!(purge_deleted(&c, now_ms()).unwrap(), Purged::default());
    }

    #[test]
    fn purge_ignores_recent_deletions() {
        let c = db();
        let p = create_project(&c, "p").unwrap();
        delete_project(&c, p.id).unwrap();
        assert_eq!(purge_deleted(&c, 0).unwrap(), Purged::default());
        restore_project(&c, p.id).unwrap();
    }

    #[test]
    fn delete_all_clears_projects_and_files() {
        let c = db();
        let p = create_project(&c, "p").unwrap();
        chat::create_conversation_in(&c, "in", Some(p.id)).unwrap();
        add_file(&c, p.id, new_file("a"), 10).unwrap();
        delete_project(&c, p.id).unwrap();
        purge_deleted(&c, now_ms()).unwrap();
        create_project(&c, "q").unwrap();
        chat::delete_all(&c).unwrap();
        for t in [
            "store_projects",
            "store_project_files",
            "store_tombstones",
            "store_conversations",
        ] {
            let n: i64 = c
                .query_row(&format!("SELECT COUNT(*) FROM {t}"), [], |r| r.get(0))
                .unwrap();
            assert_eq!(n, 0, "{t}");
        }
    }

    #[test]
    fn upgrading_keeps_old_tombstones_as_chat_tombstones() {
        // A database from before projects: three store migrations and a tombstone.
        let mut c = Connection::open_in_memory().unwrap();
        let old = &chat::MIGRATIONS[..3];
        crate::run_migrations(&mut c, &[crate::MigrationSet::new("store", old)]).unwrap();
        let id = Id::new();
        c.execute(
            "INSERT INTO store_tombstones (id, deleted_at) VALUES (?1, 5)",
            [id.to_string()],
        )
        .unwrap();
        crate::run_migrations(
            &mut c,
            &[crate::MigrationSet::new("store", chat::MIGRATIONS)],
        )
        .unwrap();
        assert_eq!(chat::list_tombstones(&c).unwrap(), vec![(id, 5)]);
        assert!(file_tombstones(&c).unwrap().is_empty());
    }
}
