//! Files attached to a chat (#370): data files (CSV and spreadsheets) the data tools can
//! read in any chat, not only in a project. Picked in a native dialog from Rust (the webview
//! never chooses a path), copied in with the project files' size cap and a SHA-256, kept
//! under `<data dir>/chat-files/<conversation id>/<file id>`, and deleted with the chat and
//! by "Delete all local data". App-owned table `app_chat_files`.
//!
//! Only data files for now (the approved ChatAttachments design). A file is checked at attach
//! time to read as a table, so a chip is never a broken file. Removing is deferred: the row
//! is hidden at once and the copy kept for [`UNDO_MS`], so Undo can bring it back and an
//! answer already running can still read it; after that the copy is deleted and a tool call
//! for it says it was removed.

use crate::error::{IpcError, IpcResult};
use crate::projects::{ImportError, copy_in, display_name, media_type};
use crate::state::AppState;
use dn_core::Id;
use dn_store::Connection;
use rusqlite::params;
use serde::Serialize;
use std::path::{Path, PathBuf};

/// Folder of the local copies, inside the data folder.
pub const FILES_DIR: &str = "chat-files";
/// Most files one chat holds.
pub const MAX_FILES: u64 = 20;
/// How long a removed file can be brought back (the Undo toast's 10 seconds).
pub const UNDO_MS: i64 = 10_000;

pub fn migrations() -> &'static [&'static str] {
    &[
        "CREATE TABLE app_chat_files (
        id              TEXT    PRIMARY KEY NOT NULL,
        conversation_id TEXT    NOT NULL,
        name            TEXT    NOT NULL,
        media_type      TEXT    NOT NULL,
        size            INTEGER NOT NULL,
        sha256          TEXT    NOT NULL,
        created_at      INTEGER NOT NULL
    );
    CREATE INDEX app_chat_files_conversation ON app_chat_files (conversation_id);",
        // Deferred removal (Undo): set when removed; the copy is deleted after UNDO_MS.
        "ALTER TABLE app_chat_files ADD COLUMN removed_at INTEGER;",
    ]
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ChatFile {
    pub id: Id,
    pub conversation_id: Id,
    /// Display name (the picked file's name). Plain text; never a path.
    pub name: String,
    pub media_type: String,
    pub size: u64,
    pub sha256: String,
    pub created_at: i64,
}

/// The chat's folder of copies.
pub fn chat_dir(root: &Path, conversation_id: Id) -> PathBuf {
    root.join(conversation_id.to_string())
}

/// A file's local copy.
pub fn file_path(root: &Path, f: &ChatFile) -> PathBuf {
    chat_dir(root, f.conversation_id).join(f.id.to_string())
}

fn parse_id(s: String) -> rusqlite::Result<Id> {
    serde_json::from_value(serde_json::Value::String(s)).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e))
    })
}

/// The chat's files, oldest first (not removed ones).
pub fn list(conn: &Connection, conversation_id: Id) -> dn_store::Result<Vec<ChatFile>> {
    select(conn, conversation_id, "removed_at IS NULL", None)
}

/// The files the data tools may read: the chat's files, and ones removed less than
/// [`UNDO_MS`] ago (an answer already running keeps reading them during Undo).
pub fn readable(
    conn: &Connection,
    conversation_id: Id,
    now: i64,
) -> dn_store::Result<Vec<ChatFile>> {
    select(
        conn,
        conversation_id,
        "(removed_at IS NULL OR removed_at > ?2)",
        Some(now - UNDO_MS),
    )
}

/// Names of files removed from the chat, for a clear error when one is asked for.
pub fn removed_names(conn: &Connection, conversation_id: Id) -> dn_store::Result<Vec<String>> {
    Ok(
        select(conn, conversation_id, "removed_at IS NOT NULL", None)?
            .into_iter()
            .map(|f| f.name)
            .collect(),
    )
}

fn select(
    conn: &Connection,
    conversation_id: Id,
    filter: &str,
    since: Option<i64>,
) -> dn_store::Result<Vec<ChatFile>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT id, conversation_id, name, media_type, size, sha256, created_at
         FROM app_chat_files WHERE conversation_id = ?1 AND {filter}
         ORDER BY removed_at IS NOT NULL, created_at, id"
    ))?;
    let row = |r: &rusqlite::Row<'_>| {
        Ok(ChatFile {
            id: parse_id(r.get(0)?)?,
            conversation_id: parse_id(r.get(1)?)?,
            name: r.get(2)?,
            media_type: r.get(3)?,
            size: r.get::<_, i64>(4)?.max(0) as u64,
            sha256: r.get(5)?,
            created_at: r.get(6)?,
        })
    };
    let rows = match since {
        Some(t) => stmt
            .query_map(params![conversation_id.to_string(), t], row)?
            .collect::<rusqlite::Result<Vec<_>>>()?,
        None => stmt
            .query_map([conversation_id.to_string()], row)?
            .collect::<rusqlite::Result<Vec<_>>>()?,
    };
    Ok(rows)
}

/// Deletes the copies of files removed more than [`UNDO_MS`] ago (the rows stay, so a tool
/// call for one says it was removed). Run by every chat-files command and at start.
pub fn expire_removed(conn: &Connection, root: &Path, now: i64) -> dn_store::Result<()> {
    let mut stmt = conn.prepare(
        "SELECT id, conversation_id FROM app_chat_files WHERE removed_at IS NOT NULL AND removed_at <= ?1",
    )?;
    let old: Vec<(String, String)> = stmt
        .query_map([now - UNDO_MS], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;
    for (id, conv) in old {
        let p = root.join(&conv).join(&id);
        if let Err(e) = std::fs::remove_file(&p)
            && e.kind() != std::io::ErrorKind::NotFound
        {
            tracing::warn!("could not delete a removed chat file's copy: {e}");
        }
    }
    Ok(())
}

/// Whether the chat has a file the data tools read (decides whether they're offered).
pub fn has_data_files(conn: &Connection, conversation_id: Id) -> bool {
    list(conn, conversation_id)
        .map(|fs| fs.iter().any(|f| dn_data::tool::reads(&f.name)))
        .unwrap_or(false)
}

#[derive(Debug, Serialize)]
pub struct SkippedFile {
    pub name: String,
    pub reason: String,
}

#[derive(Debug, Serialize)]
pub struct AddedFiles {
    /// The user closed the dialog without picking anything.
    pub cancelled: bool,
    pub added: Vec<ChatFile>,
    pub skipped: Vec<SkippedFile>,
    /// The chat the files are in: the one given, or the one created for them (a new chat has
    /// no conversation until something is in it). `None` when cancelled without a chat.
    pub conversation_id: Option<Id>,
}

fn root(state: &AppState) -> IpcResult<PathBuf> {
    state
        .chat_files_root()
        .map(Path::to_path_buf)
        .ok_or_else(|| IpcError::new("unavailable", "Chat files aren't available here."))
}

/// [`add`] into `conversation_id`, or into a new chat (in `project_id`, if given) created for
/// the picked files. The new chat stays even if every file was refused: the person picked
/// something, and the reasons show in it.
pub fn add_to(
    state: &AppState,
    conversation_id: Option<Id>,
    project_id: Option<Id>,
    paths: &[PathBuf],
) -> IpcResult<AddedFiles> {
    let c = match conversation_id {
        Some(c) => c,
        None => state.create_conversation_in(project_id)?.id,
    };
    add(state, c, paths)
}

/// Copies picked files into the chat, in order; files that aren't data files or are past
/// the limits are skipped with a reason.
pub fn add(state: &AppState, conversation_id: Id, paths: &[PathBuf]) -> IpcResult<AddedFiles> {
    let root = root(state)?;
    // Fails with "not found" if the chat is gone.
    dn_store::chat::get_conversation(&*state.db()?, conversation_id)?;
    let mut out = AddedFiles {
        cancelled: false,
        added: vec![],
        skipped: vec![],
        conversation_id: Some(conversation_id),
    };
    for p in paths {
        let name = display_name(p);
        let skip = |reason: &str| SkippedFile {
            name: name.clone(),
            reason: reason.into(),
        };
        if !dn_data::tool::reads(&name) {
            out.skipped.push(skip(
                "only tables (CSV, TSV, TXT) and spreadsheets can be attached for now.",
            ));
            continue;
        }
        let existing = list(&*state.db()?, conversation_id)?;
        // One file per name, so the data tools never have to pick between two silently.
        if existing.iter().any(|f| f.name.eq_ignore_ascii_case(&name)) {
            out.skipped.push(skip(
                "a file with this name is already attached. Remove it first, or rename the new one.",
            ));
            continue;
        }
        let count = existing.len() as u64;
        if count >= MAX_FILES {
            out.skipped
                .push(skip(&format!("a chat holds at most {MAX_FILES} files.")));
            continue;
        }
        let dir = chat_dir(&root, conversation_id);
        match copy_in(&dir, p) {
            Ok((id, size, sha256)) => {
                let copy = dir.join(id.to_string());
                // Checked now, so a chip is never a file the data tools can't read.
                let check = std::fs::read(&copy)
                    .map_err(|_| "it couldn't be read.".to_string())
                    .and_then(|b| {
                        dn_data::read(&name, &b, &dn_data::Limits::default())
                            .map(|_| ())
                            .map_err(|e| match e {
                                dn_data::DataError::Empty => {
                                    "it can't be read as a table (the file is empty).".to_string()
                                }
                                dn_data::DataError::TooLarge(_) => "it's larger than 50 MB.".to_string(),
                                dn_data::DataError::Unreadable(why) => {
                                    format!("it can't be read as a table ({why}).")
                                }
                                dn_data::DataError::Unsupported(_) => {
                                    "only tables (CSV, TSV, TXT) and spreadsheets can be attached for now."
                                        .to_string()
                                }
                            })
                    });
                if let Err(reason) = check {
                    let _ = std::fs::remove_file(&copy);
                    out.skipped.push(skip(&reason));
                    continue;
                }
                let f = ChatFile {
                    id,
                    conversation_id,
                    media_type: media_type(&name).into(),
                    name: name.clone(),
                    size,
                    sha256,
                    created_at: dn_store::now_ms(),
                };
                let stored = state.db()?.execute(
                    "INSERT INTO app_chat_files (id, conversation_id, name, media_type, size, sha256, created_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                    params![
                        f.id.to_string(),
                        f.conversation_id.to_string(),
                        f.name,
                        f.media_type,
                        i64::try_from(f.size).unwrap_or(i64::MAX),
                        f.sha256,
                        f.created_at
                    ],
                );
                match stored {
                    Ok(_) => out.added.push(f),
                    Err(e) => {
                        let _ = std::fs::remove_file(file_path(&root, &f));
                        tracing::warn!("could not record a chat file: {e}");
                        out.skipped.push(skip("it couldn't be saved."));
                    }
                }
            }
            Err(e) => {
                if let ImportError::Unreadable(io) = &e {
                    tracing::warn!("could not add a chat file: {io}");
                }
                out.skipped.push(skip(match e {
                    ImportError::NotAFile => "it isn't a file.",
                    ImportError::TooLarge => "it's larger than 50 MB.",
                    ImportError::Unreadable(_) => "it couldn't be read.",
                    ImportError::Store(_) => "it couldn't be saved.",
                }));
            }
        }
    }
    Ok(out)
}

/// A removed file, until [`UNDO_MS`] has passed.
#[derive(Debug, Serialize)]
pub struct RemovedFile {
    pub id: Id,
    pub name: String,
    /// Until when [`restore`] brings it back (Unix ms).
    pub undo_until: i64,
}

/// Removes one file from a chat: hidden at once, its copy kept for [`UNDO_MS`] so Undo can
/// bring it back and an answer already running can still read it. `None` if it wasn't there.
pub fn remove(
    state: &AppState,
    conversation_id: Id,
    file_id: Id,
) -> IpcResult<Option<RemovedFile>> {
    let root = root(state)?;
    let db = state.db()?;
    let now = dn_store::now_ms();
    expire_removed(&db, &root, now)?;
    let Some(f) = list(&db, conversation_id)?
        .into_iter()
        .find(|f| f.id == file_id)
    else {
        return Ok(None);
    };
    db.execute(
        "UPDATE app_chat_files SET removed_at = ?3 WHERE id = ?1 AND conversation_id = ?2 AND removed_at IS NULL",
        params![f.id.to_string(), conversation_id.to_string(), now],
    )
    .map_err(dn_store::StoreError::from)?;
    Ok(Some(RemovedFile {
        id: f.id,
        name: f.name,
        undo_until: now + UNDO_MS,
    }))
}

/// Undo: brings back a file removed less than [`UNDO_MS`] ago, as it was.
pub fn restore(state: &AppState, conversation_id: Id, file_id: Id) -> IpcResult<ChatFile> {
    let root = root(state)?;
    let db = state.db()?;
    let now = dn_store::now_ms();
    let gone = || IpcError::new("not_found", "This file can't be brought back any more.");
    let Some(f) = select(
        &db,
        conversation_id,
        "removed_at IS NOT NULL AND removed_at > ?2",
        Some(now - UNDO_MS),
    )?
    .into_iter()
    .find(|f| f.id == file_id) else {
        return Err(gone());
    };
    if !file_path(&root, &f).is_file() {
        return Err(gone());
    }
    if list(&db, conversation_id)?
        .iter()
        .any(|x| x.name.eq_ignore_ascii_case(&f.name))
    {
        return Err(IpcError::new(
            "conflict",
            "A file with this name was attached since. Remove it first.",
        ));
    }
    db.execute(
        "UPDATE app_chat_files SET removed_at = NULL WHERE id = ?1 AND conversation_id = ?2",
        params![f.id.to_string(), conversation_id.to_string()],
    )
    .map_err(dn_store::StoreError::from)?;
    Ok(f)
}

/// Forgets a purged chat's files: rows and the folder of copies. Idempotent.
pub fn forget_chat(
    conn: &Connection,
    root: Option<&Path>,
    conversation_id: Id,
) -> dn_store::Result<()> {
    conn.execute(
        "DELETE FROM app_chat_files WHERE conversation_id = ?1",
        [conversation_id.to_string()],
    )?;
    if let Some(root) = root {
        let dir = chat_dir(root, conversation_id);
        if let Err(e) = std::fs::remove_dir_all(&dir)
            && e.kind() != std::io::ErrorKind::NotFound
        {
            return Err(dn_store::StoreError::Invalid(format!(
                "the chat's files couldn't be deleted: {e}"
            )));
        }
    }
    Ok(())
}

/// Delete all local data: every row and every copy.
pub fn delete_all(conn: &Connection, root: Option<&Path>) -> dn_store::Result<()> {
    conn.execute("DELETE FROM app_chat_files", [])?;
    if let Some(root) = root
        && let Err(e) = std::fs::remove_dir_all(root)
        && e.kind() != std::io::ErrorKind::NotFound
    {
        return Err(dn_store::StoreError::Invalid(format!(
            "chat files couldn't be deleted: {e}"
        )));
    }
    Ok(())
}

type AppStateRef<'a> = tauri::State<'a, std::sync::Arc<AppState>>;

/// The chat's files.
#[tauri::command]
pub async fn list_chat_files(
    state: AppStateRef<'_>,
    conversation_id: Id,
) -> IpcResult<Vec<ChatFile>> {
    crate::commands::blocking(&state, move |s| {
        let db = s.db()?;
        if let Some(root) = s.chat_files_root() {
            expire_removed(&db, root, dn_store::now_ms())?;
        }
        Ok(list(&db, conversation_id)?)
    })
    .await
}

/// Opens the system file picker (from Rust; the webview never chooses a path) and copies
/// the picked data files into the chat.
#[tauri::command]
pub async fn add_chat_files(
    app: tauri::AppHandle,
    window: tauri::Window,
    state: AppStateRef<'_>,
    conversation_id: Option<Id>,
    project_id: Option<Id>,
) -> IpcResult<AddedFiles> {
    use tauri_plugin_dialog::DialogExt;
    // A given chat must exist; a new one is only created once something was picked, so a
    // cancelled picker leaves no empty chat behind.
    if let Some(c) = conversation_id {
        crate::commands::blocking(&state, move |s| {
            dn_store::chat::get_conversation(&*s.db()?, c)
                .map(|_| ())
                .map_err(Into::into)
        })
        .await?;
    }
    let (tx, rx) = tokio::sync::oneshot::channel();
    app.dialog()
        .file()
        .set_parent(&window)
        .add_filter(
            "Data files",
            &["csv", "tsv", "txt", "xlsx", "xlsm", "xlsb", "xls", "ods"],
        )
        .pick_files(move |picked| {
            let _ = tx.send(picked);
        });
    let picked = rx.await.ok().flatten();
    let picked = picked.filter(|p| !p.is_empty());
    let Some(picked) = picked else {
        return Ok(AddedFiles {
            cancelled: true,
            added: vec![],
            skipped: vec![],
            conversation_id,
        });
    };
    let paths: Vec<PathBuf> = picked
        .into_iter()
        .filter_map(|p| p.into_path().ok())
        .collect();
    crate::commands::blocking(&state, move |s| {
        add_to(s, conversation_id, project_id, &paths)
    })
    .await
}

/// Removes a file (Undo with `restore_chat_file` for [`UNDO_MS`]). The same shape as
/// `remove_project_file`; a file that isn't there is "not found".
#[tauri::command]
pub async fn remove_chat_file(
    state: AppStateRef<'_>,
    conversation_id: Id,
    file_id: Id,
) -> IpcResult<()> {
    crate::commands::blocking(&state, move |s| {
        match remove(s, conversation_id, file_id)? {
            Some(_) => Ok(()),
            None => Err(IpcError::new("not_found", "This file isn't in this chat.")),
        }
    })
    .await
}

/// Undo of `remove_chat_file` (like `restore_project_file`).
#[tauri::command]
pub async fn restore_chat_file(
    state: AppStateRef<'_>,
    conversation_id: Id,
    file_id: Id,
) -> IpcResult<()> {
    crate::commands::blocking(&state, move |s| {
        restore(s, conversation_id, file_id).map(|_| ())
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state() -> (std::sync::Arc<AppState>, PathBuf) {
        let dir = std::env::temp_dir().join(format!("dn-chat-files-{}", Id::new()));
        (
            AppState::open_for_tests(&dir.join("db.sqlite3")).unwrap(),
            dir,
        )
    }

    fn src(dir: &Path, name: &str, content: &[u8]) -> PathBuf {
        let s = dir.join("src");
        std::fs::create_dir_all(&s).unwrap();
        let p = s.join(name);
        std::fs::write(&p, content).unwrap();
        p
    }

    #[test]
    fn data_files_are_copied_in_and_others_are_refused_with_a_reason() {
        let (st, dir) = state();
        let conv = st.create_conversation().unwrap();
        let csv = src(&dir, "sales.csv", b"a,b\n1,2\n");
        let pdf = src(&dir, "notes.pdf", b"%PDF");
        let r = add(&st, conv.id, &[csv, pdf]).unwrap();
        assert_eq!(r.added.len(), 1);
        assert_eq!(r.added[0].name, "sales.csv");
        assert_eq!(r.added[0].size, 8);
        assert_eq!(r.skipped.len(), 1);
        assert!(
            r.skipped[0]
                .reason
                .starts_with("only tables (CSV, TSV, TXT)")
        );
        let root = st.chat_files_root().unwrap();
        assert_eq!(
            std::fs::read(file_path(root, &r.added[0])).unwrap(),
            b"a,b\n1,2\n"
        );
        assert!(has_data_files(&st.db().unwrap(), conv.id));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn files_go_with_their_chat_and_with_delete_all() {
        let (st, dir) = state();
        let (a, b) = (
            st.create_conversation().unwrap(),
            st.create_conversation().unwrap(),
        );
        let csv = src(&dir, "x.csv", b"a\n1\n");
        let fa = add(&st, a.id, std::slice::from_ref(&csv))
            .unwrap()
            .added
            .remove(0);
        let fb = add(&st, b.id, &[csv]).unwrap().added.remove(0);
        let root = st.chat_files_root().unwrap().to_path_buf();
        // Removing one file: hidden at once, the copy kept for Undo, deleted after.
        let r = remove(&st, a.id, fa.id).unwrap().unwrap();
        assert!(list(&st.db().unwrap(), a.id).unwrap().is_empty());
        assert!(file_path(&root, &fa).exists(), "kept for Undo");
        expire_removed(&st.db().unwrap(), &root, r.undo_until + 1).unwrap();
        assert!(!file_path(&root, &fa).exists());
        assert!(remove(&st, a.id, fa.id).unwrap().is_none(), "already gone");
        // Another chat's file can't be removed through this chat.
        assert!(remove(&st, a.id, fb.id).unwrap().is_none());
        // Purging chat b deletes its rows and folder.
        forget_chat(&st.db().unwrap(), Some(&root), b.id).unwrap();
        assert!(list(&st.db().unwrap(), b.id).unwrap().is_empty());
        assert!(!chat_dir(&root, b.id).exists());
        // Delete all.
        add(&st, a.id, &[src(&dir, "y.csv", b"a\n1\n")]).unwrap();
        delete_all(&st.db().unwrap(), Some(&root)).unwrap();
        assert!(list(&st.db().unwrap(), a.id).unwrap().is_empty());
        assert!(!root.exists());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_second_file_with_the_same_name_is_refused() {
        let (st, dir) = state();
        let conv = st.create_conversation().unwrap();
        let a = src(&dir, "Sales.csv", b"a\n1\n");
        add(&st, conv.id, &[a]).unwrap();
        let other = dir.join("other");
        std::fs::create_dir_all(&other).unwrap();
        std::fs::write(other.join("sales.CSV"), b"a\n2\n").unwrap();
        let r = add(&st, conv.id, &[other.join("sales.CSV")]).unwrap();
        assert!(r.added.is_empty());
        assert!(r.skipped[0].reason.contains("already attached"));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_chat_holds_at_most_twenty_files_and_a_gone_chat_none() {
        let (st, dir) = state();
        let conv = st.create_conversation().unwrap();
        let paths: Vec<PathBuf> = (0..MAX_FILES + 1)
            .map(|i| src(&dir, &format!("f{i}.csv"), b"a\n1\n"))
            .collect();
        let r = add(&st, conv.id, &paths).unwrap();
        assert_eq!(r.added.len() as u64, MAX_FILES);
        assert!(r.skipped[0].reason.contains("at most 20"));
        assert_eq!(
            add(&st, Id::new(), &paths[..1]).unwrap_err().code,
            "not_found"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn undo_brings_a_removed_file_back_and_a_running_answer_can_still_read_it() {
        let (st, dir) = state();
        let c = st.create_conversation().unwrap();
        let f = add(&st, c.id, &[src(&dir, "sales.csv", b"a,b\n1,2\n")])
            .unwrap()
            .added
            .remove(0);
        let r = remove(&st, c.id, f.id).unwrap().unwrap();
        assert_eq!(r.name, "sales.csv");
        let db = st.db().unwrap();
        let now = dn_store::now_ms();
        assert_eq!(
            readable(&db, c.id, now).unwrap().len(),
            1,
            "still readable during Undo"
        );
        assert!(readable(&db, c.id, r.undo_until + 1).unwrap().is_empty());
        drop(db);
        let back = restore(&st, c.id, f.id).unwrap();
        assert_eq!(back.id, f.id);
        assert_eq!(list(&st.db().unwrap(), c.id).unwrap().len(), 1);
        // After the Undo time, it can't come back, and its name is known as removed.
        let r = remove(&st, c.id, f.id).unwrap().unwrap();
        st.db()
            .unwrap()
            .execute(
                "UPDATE app_chat_files SET removed_at = ?1 WHERE id = ?2",
                rusqlite::params![r.undo_until - 2 * UNDO_MS, f.id.to_string()],
            )
            .unwrap();
        assert!(restore(&st, c.id, f.id).is_err());
        assert_eq!(
            removed_names(&st.db().unwrap(), c.id).unwrap(),
            ["sales.csv"]
        );
        // A new file with the same name can be attached once the old one is removed, and it
        // comes first for the data tools, even while the old one is still in its Undo time.
        let new = add(&st, c.id, &[src(&dir, "sales.csv", b"x\n1\n")])
            .unwrap()
            .added
            .remove(0);
        st.db()
            .unwrap()
            .execute(
                "UPDATE app_chat_files SET removed_at = ?1 WHERE id = ?2",
                rusqlite::params![dn_store::now_ms(), f.id.to_string()],
            )
            .unwrap();
        let r = readable(&st.db().unwrap(), c.id, dn_store::now_ms()).unwrap();
        assert_eq!((r.len(), r[0].id), (2, new.id));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_file_that_doesnt_read_as_a_table_isnt_attached_and_reasons_follow_the_design() {
        let (st, dir) = state();
        let c = st.create_conversation().unwrap();
        let out = add(
            &st,
            c.id,
            &[
                src(&dir, "export.csv", b""),
                src(&dir, "broken.xlsx", b"not a workbook"),
                src(&dir, "notes.pdf", b"%PDF"),
            ],
        )
        .unwrap();
        assert!(out.added.is_empty());
        let reasons: Vec<&str> = out.skipped.iter().map(|s| s.reason.as_str()).collect();
        assert_eq!(
            reasons[0],
            "it can't be read as a table (the file is empty)."
        );
        assert!(
            reasons[1].starts_with("it can't be read as a table ("),
            "{}",
            reasons[1]
        );
        assert_eq!(
            reasons[2],
            "only tables (CSV, TSV, TXT) and spreadsheets can be attached for now."
        );
        // Nothing left behind for the refused files.
        let root = st.chat_files_root().unwrap();
        let left = std::fs::read_dir(chat_dir(root, c.id))
            .map(|d| d.count())
            .unwrap_or(0);
        assert_eq!(left, 0);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn files_picked_in_a_new_chat_create_it_in_its_project() {
        let (st, dir) = state();
        let p = dn_store::project::create_project(&st.db().unwrap(), "Trips").unwrap();
        let out = add_to(&st, None, Some(p.id), &[src(&dir, "a.csv", b"x\n1\n")]).unwrap();
        let c = out.conversation_id.unwrap();
        assert_eq!(out.added.len(), 1);
        let conv = dn_store::chat::get_conversation(&st.db().unwrap(), c).unwrap();
        assert_eq!(conv.project_id, Some(p.id));
        // An existing chat is used as given.
        let again = add_to(&st, Some(c), None, &[src(&dir, "b.csv", b"x\n1\n")]).unwrap();
        assert_eq!(again.conversation_id, Some(c));
        let _ = std::fs::remove_dir_all(dir);
    }
}
