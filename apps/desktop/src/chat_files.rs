//! Files attached to a chat (#370): data files (CSV and spreadsheets) the data tools can
//! read in any chat, not only in a project. Picked in a native dialog from Rust (the webview
//! never chooses a path), copied in with the project files' size cap and a SHA-256, kept
//! under `<data dir>/chat-files/<conversation id>/<file id>`, and deleted with the chat and
//! by "Delete all local data". App-owned table `app_chat_files`.
//!
//! Only data files for now: other kinds (documents, images) come with the attach UI, which
//! needs an approved design first.

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

pub fn migrations() -> &'static [&'static str] {
    &["CREATE TABLE app_chat_files (
        id              TEXT    PRIMARY KEY NOT NULL,
        conversation_id TEXT    NOT NULL,
        name            TEXT    NOT NULL,
        media_type      TEXT    NOT NULL,
        size            INTEGER NOT NULL,
        sha256          TEXT    NOT NULL,
        created_at      INTEGER NOT NULL
    );
    CREATE INDEX app_chat_files_conversation ON app_chat_files (conversation_id);"]
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

/// The chat's files, oldest first.
pub fn list(conn: &Connection, conversation_id: Id) -> dn_store::Result<Vec<ChatFile>> {
    let mut stmt = conn.prepare(
        "SELECT id, conversation_id, name, media_type, size, sha256, created_at
         FROM app_chat_files WHERE conversation_id = ?1 ORDER BY created_at, id",
    )?;
    let rows = stmt.query_map([conversation_id.to_string()], |r| {
        Ok(ChatFile {
            id: parse_id(r.get(0)?)?,
            conversation_id: parse_id(r.get(1)?)?,
            name: r.get(2)?,
            media_type: r.get(3)?,
            size: r.get::<_, i64>(4)?.max(0) as u64,
            sha256: r.get(5)?,
            created_at: r.get(6)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
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
}

fn root(state: &AppState) -> IpcResult<PathBuf> {
    state
        .chat_files_root()
        .map(Path::to_path_buf)
        .ok_or_else(|| IpcError::new("unavailable", "Chat files aren't available here."))
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
    };
    for p in paths {
        let name = display_name(p);
        let skip = |reason: &str| SkippedFile {
            name: name.clone(),
            reason: reason.into(),
        };
        if !dn_data::tool::reads(&name) {
            out.skipped.push(skip(
                "Only CSV and spreadsheet files can be attached to a chat for now.",
            ));
            continue;
        }
        let count = list(&*state.db()?, conversation_id)?.len() as u64;
        if count >= MAX_FILES {
            out.skipped
                .push(skip(&format!("A chat holds at most {MAX_FILES} files.")));
            continue;
        }
        let dir = chat_dir(&root, conversation_id);
        match copy_in(&dir, p) {
            Ok((id, size, sha256)) => {
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
                        out.skipped.push(skip("It couldn't be saved."));
                    }
                }
            }
            Err(e) => {
                if let ImportError::Unreadable(io) = &e {
                    tracing::warn!("could not add a chat file: {io}");
                }
                out.skipped.push(skip(&e.to_string()));
            }
        }
    }
    Ok(out)
}

/// Removes one file from a chat (row and copy). `false` if it wasn't there.
pub fn remove(state: &AppState, conversation_id: Id, file_id: Id) -> IpcResult<bool> {
    let root = root(state)?;
    let db = state.db()?;
    let f: Option<ChatFile> = list(&db, conversation_id)?
        .into_iter()
        .find(|f| f.id == file_id);
    let Some(f) = f else { return Ok(false) };
    db.execute(
        "DELETE FROM app_chat_files WHERE id = ?1 AND conversation_id = ?2",
        params![f.id.to_string(), conversation_id.to_string()],
    )
    .map_err(dn_store::StoreError::from)?;
    if let Err(e) = std::fs::remove_file(file_path(&root, &f))
        && e.kind() != std::io::ErrorKind::NotFound
    {
        tracing::warn!("could not delete a chat file's copy: {e}");
    }
    Ok(true)
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
    crate::commands::blocking(&state, move |s| Ok(list(&*s.db()?, conversation_id)?)).await
}

/// Opens the system file picker (from Rust; the webview never chooses a path) and copies
/// the picked data files into the chat.
#[tauri::command]
pub async fn add_chat_files(
    app: tauri::AppHandle,
    window: tauri::Window,
    state: AppStateRef<'_>,
    conversation_id: Id,
) -> IpcResult<AddedFiles> {
    use tauri_plugin_dialog::DialogExt;
    crate::commands::blocking(&state, move |s| {
        dn_store::chat::get_conversation(&*s.db()?, conversation_id)
            .map(|_| ())
            .map_err(Into::into)
    })
    .await?;
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
    let Some(picked) = picked else {
        return Ok(AddedFiles {
            cancelled: true,
            added: vec![],
            skipped: vec![],
        });
    };
    let paths: Vec<PathBuf> = picked
        .into_iter()
        .filter_map(|p| p.into_path().ok())
        .collect();
    crate::commands::blocking(&state, move |s| add(s, conversation_id, &paths)).await
}

#[tauri::command]
pub async fn remove_chat_file(
    state: AppStateRef<'_>,
    conversation_id: Id,
    file_id: Id,
) -> IpcResult<bool> {
    crate::commands::blocking(&state, move |s| remove(s, conversation_id, file_id)).await
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
        assert!(r.skipped[0].reason.contains("Only CSV and spreadsheet"));
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
        // Removing one file.
        assert!(remove(&st, a.id, fa.id).unwrap());
        assert!(!file_path(&root, &fa).exists());
        assert!(!remove(&st, a.id, fa.id).unwrap(), "already gone");
        // Another chat's file can't be removed through this chat.
        assert!(!remove(&st, a.id, fb.id).unwrap());
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
}
