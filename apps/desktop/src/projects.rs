//! Projects (T-PRJ-01): chats, files and standing instructions grouped together.
//!
//! The rows live in dn-store ([`dn_store::project`]). This module owns the local copies of
//! project files under `<data dir>/project-files/<project id>/<file id>`: files are picked
//! in a native dialog shown from Rust (the webview never sees a path it could choose),
//! copied in with a size cap and a SHA-256, and deleted when the store purges them or on
//! "Delete all local data". Indexing is dn-knowledge's job (#116); [`files_changed`] is
//! where it hooks in.

use crate::error::{IpcError, IpcResult};
use crate::state::AppState;
use dn_core::Id;
use dn_store::Connection;
use dn_store::chat as store;
use dn_store::project::{self, NewProjectFile, Project, ProjectFile, ProjectSummary};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Largest file a project takes.
pub const MAX_FILE_BYTES: u64 = 50 * 1024 * 1024;
/// Most files one project holds.
pub const MAX_FILES: u64 = 100;
/// Folder of the local copies, inside the data folder.
pub const FILES_DIR: &str = "project-files";
/// Longest display name kept for a file, in characters.
const FILE_NAME_MAX: usize = 255;

/// Media type from the file name's extension (dn-knowledge decides what it can read).
pub fn media_type(name: &str) -> &'static str {
    let ext = Path::new(name)
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
        .unwrap_or_default();
    match ext.as_str() {
        "txt" | "text" | "log" => "text/plain",
        "md" | "markdown" => "text/markdown",
        "csv" => "text/csv",
        "tsv" => "text/tab-separated-values",
        "html" | "htm" => "text/html",
        "json" => "application/json",
        "xml" => "application/xml",
        "yaml" | "yml" => "application/yaml",
        "pdf" => "application/pdf",
        "docx" => "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        "xlsx" => "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
        "pptx" => "application/vnd.openxmlformats-officedocument.presentationml.presentation",
        "odt" => "application/vnd.oasis.opendocument.text",
        "rtf" => "application/rtf",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "rs" | "py" | "js" | "ts" | "tsx" | "jsx" | "go" | "java" | "c" | "h" | "cpp" | "hpp"
        | "cs" | "rb" | "php" | "swift" | "kt" | "sh" | "sql" | "toml" | "css" => "text/plain",
        _ => "application/octet-stream",
    }
}

/// A display name from a picked path: its last component, without control or bidi
/// characters, at most [`FILE_NAME_MAX`] characters.
fn display_name(src: &Path) -> String {
    let raw = src
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let clean: String = crate::tool_review::plain_text(&raw, FILE_NAME_MAX)
        .chars()
        .filter(|c| !c.is_control())
        .collect();
    if clean.is_empty() {
        "Untitled".into()
    } else {
        clean
    }
}

/// Why a picked file wasn't added, in the user's words.
#[derive(Debug, thiserror::Error)]
pub enum ImportError {
    #[error("It isn't a file.")]
    NotAFile,
    #[error("It's larger than 50 MB.")]
    TooLarge,
    #[error("It couldn't be read.")]
    Unreadable(#[source] std::io::Error),
    #[error("It couldn't be saved. {0}")]
    Store(dn_store::StoreError),
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes.iter().fold(String::with_capacity(64), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    })
}

/// Creates `dir` (and parents); on Unix it is `0700` like the data folder.
fn private_dir(dir: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(dir)
    }
    #[cfg(not(unix))]
    {
        std::fs::create_dir_all(dir)
    }
}

fn create_private_file(path: &Path) -> std::io::Result<std::fs::File> {
    let mut o = std::fs::OpenOptions::new();
    o.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        o.mode(0o600);
    }
    o.open(path)
}

/// Copies `src` into the project's folder under `root` and records it. The copy is
/// written to `<id>.part` and renamed when complete, so a crash never leaves a half file
/// under a real id; [`sweep`] removes leftovers.
pub fn import(
    conn: &Connection,
    root: &Path,
    project_id: Id,
    src: &Path,
) -> Result<ProjectFile, ImportError> {
    project::get_project(conn, project_id).map_err(ImportError::Store)?;
    let mut input = std::fs::File::open(src).map_err(ImportError::Unreadable)?;
    // Check the opened handle, not the path, so the file can't be swapped in between.
    let meta = input.metadata().map_err(ImportError::Unreadable)?;
    if !meta.is_file() {
        return Err(ImportError::NotAFile);
    }
    if meta.len() > MAX_FILE_BYTES {
        return Err(ImportError::TooLarge);
    }
    let dir = project::project_dir(root, project_id);
    private_dir(&dir).map_err(ImportError::Unreadable)?;
    let id = Id::new();
    let part = dir.join(format!("{id}.part"));
    let fin = dir.join(id.to_string());
    let copied = (|| -> Result<(u64, String), ImportError> {
        let mut out = create_private_file(&part).map_err(ImportError::Unreadable)?;
        let mut hash = Sha256::new();
        let mut buf = vec![0u8; 64 * 1024];
        let mut total = 0u64;
        // Read at most one byte past the cap: the file may have grown since `metadata`.
        let mut limited = (&mut input).take(MAX_FILE_BYTES + 1);
        loop {
            let n = limited.read(&mut buf).map_err(ImportError::Unreadable)?;
            if n == 0 {
                break;
            }
            total += n as u64;
            if total > MAX_FILE_BYTES {
                return Err(ImportError::TooLarge);
            }
            hash.update(&buf[..n]);
            out.write_all(&buf[..n]).map_err(ImportError::Unreadable)?;
        }
        out.sync_all().map_err(ImportError::Unreadable)?;
        std::fs::rename(&part, &fin).map_err(ImportError::Unreadable)?;
        Ok((total, hex(&hash.finalize())))
    })();
    let (size, sha256) = match copied {
        Ok(v) => v,
        Err(e) => {
            let _ = std::fs::remove_file(&part);
            return Err(e);
        }
    };
    let name = display_name(src);
    let new = NewProjectFile {
        id,
        media_type: media_type(&name).into(),
        name,
        size,
        sha256,
        source_path: Some(src.display().to_string()),
    };
    project::add_file(conn, project_id, new, MAX_FILES).map_err(|e| {
        let _ = std::fs::remove_file(&fin);
        ImportError::Store(e)
    })
}

/// Deletes the local copies of purged files and the folders of purged projects.
pub fn remove_copies(root: &Path, purged: &project::Purged) {
    for (p, f) in &purged.files {
        let path = project::project_dir(root, *p).join(f.to_string());
        match std::fs::remove_file(&path) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => eprintln!("could not delete a project file copy: {e}"),
        }
    }
    for p in &purged.projects {
        match std::fs::remove_dir_all(project::project_dir(root, *p)) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => eprintln!("could not delete a project's files folder: {e}"),
        }
    }
}

/// Removes anything under `root` the store doesn't know: `.part` leftovers from a crash,
/// copies whose row never got written, folders of projects that no longer exist.
pub fn sweep(conn: &Connection, root: &Path) -> dn_store::Result<()> {
    use std::collections::HashSet;
    let known: HashSet<String> = project::stored_files(conn)?
        .into_iter()
        .map(|(p, f)| format!("{p}/{f}"))
        .collect();
    let projects: HashSet<String> = project::stored_projects(conn)?
        .into_iter()
        .map(|p| p.to_string())
        .collect();
    let Ok(dirs) = std::fs::read_dir(root) else {
        return Ok(()); // No folder yet.
    };
    for dir in dirs.flatten() {
        let path = dir.path();
        let pid = path.file_name().and_then(|n| n.to_str()).map(str::to_owned);
        let Some(pid) = pid.filter(|p| projects.contains(p)) else {
            let r = if path.is_dir() {
                std::fs::remove_dir_all(&path)
            } else {
                std::fs::remove_file(&path)
            };
            if let Err(e) = r {
                eprintln!("could not remove a stray project file: {e}");
            }
            continue;
        };
        let Ok(files) = std::fs::read_dir(&path) else {
            continue;
        };
        for f in files.flatten() {
            let name = f.file_name();
            let fid = name.to_str().unwrap_or("");
            if !known.contains(&format!("{pid}/{fid}")) {
                let p = f.path();
                let r = if p.is_dir() {
                    std::fs::remove_dir_all(&p)
                } else {
                    std::fs::remove_file(&p)
                };
                if let Err(e) = r {
                    eprintln!("could not remove a stray project file: {e}");
                }
            }
        }
    }
    Ok(())
}

/// On start, after chats are purged: purge deleted projects and files, delete their
/// copies and anything stray.
pub fn purge_on_start(conn: &Connection, root: &Path) -> dn_store::Result<()> {
    let purged = project::purge_deleted(conn, dn_store::now_ms())?;
    remove_copies(root, &purged);
    sweep(conn, root)
}

/// Called after a project's files change (added, removed, restored): dn-knowledge (#116)
/// re-indexes the project in the background.
pub fn files_changed(_state: &AppState, project_id: Id) {
    crate::knowledge::files_changed(project_id);
}

/// The project's standing instructions for the system prompt of a chat, if it has any.
pub fn instructions_for(conn: &Connection, conversation_id: Id) -> IpcResult<Option<String>> {
    let Some(pid) = store::get_conversation(conn, conversation_id)?.project_id else {
        return Ok(None);
    };
    let p = match project::get_project(conn, pid) {
        Ok(p) => p,
        Err(dn_store::StoreError::NotFound(_)) => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    if p.instructions.trim().is_empty() {
        return Ok(None);
    }
    Ok(Some(format!(
        "This chat is part of the user's project \"{}\". The user gave these standing \
         instructions for every chat in the project; follow them unless a message in this \
         chat says otherwise:\n\n{}",
        p.name, p.instructions
    )))
}

// ---- commands

type AppStateRef<'a> = tauri::State<'a, Arc<AppState>>;

fn files_root(s: &AppState) -> IpcResult<PathBuf> {
    s.project_files.clone().ok_or_else(|| {
        IpcError::new(
            "unavailable",
            "Project files aren't available in this build.",
        )
    })
}

#[tauri::command]
pub async fn list_projects(state: AppStateRef<'_>) -> IpcResult<Vec<ProjectSummary>> {
    crate::commands::blocking(&state, |s| Ok(project::list_projects(&*s.db()?)?)).await
}

#[tauri::command]
pub async fn create_project(state: AppStateRef<'_>, name: String) -> IpcResult<Project> {
    crate::commands::blocking(&state, move |s| {
        Ok(project::create_project(&*s.db()?, &name)?)
    })
    .await
}

#[tauri::command]
pub async fn update_project(
    state: AppStateRef<'_>,
    project_id: Id,
    name: String,
    instructions: String,
) -> IpcResult<Project> {
    crate::commands::blocking(&state, move |s| {
        Ok(project::update_project(
            &*s.db()?,
            project_id,
            &name,
            &instructions,
        )?)
    })
    .await
}

/// Soft delete of the project with its chats and files: Undo restores them until the next
/// start, which purges them. Returns how many chats went with it.
#[tauri::command]
pub async fn delete_project(state: AppStateRef<'_>, project_id: Id) -> IpcResult<usize> {
    let chats = crate::commands::blocking(&state, move |s| {
        Ok(project::delete_project(&*s.db()?, project_id)?)
    })
    .await?;
    for c in &chats {
        crate::chat::stop(&state, *c);
        if let Some(exec) = &state.executor {
            exec.gate().clear_conversation(&format!("conv:{c}"));
        }
    }
    let st = Arc::clone(state.inner());
    files_changed(&st, project_id);
    Ok(chats.len())
}

#[tauri::command]
pub async fn restore_project(state: AppStateRef<'_>, project_id: Id) -> IpcResult<()> {
    crate::commands::blocking(&state, move |s| {
        project::restore_project(&*s.db()?, project_id)?;
        files_changed(s, project_id);
        Ok(())
    })
    .await
}

/// Moves a chat into a project, or out of one with `None` (#136). Returns the project it
/// was in before, for Undo. The next message uses the new project's instructions.
#[tauri::command]
pub async fn move_conversation(
    state: AppStateRef<'_>,
    conversation_id: Id,
    project_id: Option<Id>,
) -> IpcResult<Option<Id>> {
    crate::commands::blocking(&state, move |s| {
        Ok(store::set_conversation_project(
            &*s.db()?,
            conversation_id,
            project_id,
        )?)
    })
    .await
}

#[tauri::command]
pub async fn list_project_files(
    state: AppStateRef<'_>,
    project_id: Id,
) -> IpcResult<Vec<ProjectFile>> {
    crate::commands::blocking(&state, move |s| {
        let db = s.db()?;
        project::get_project(&db, project_id)?;
        Ok(project::list_files(&db, project_id)?)
    })
    .await
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
    pub added: Vec<ProjectFile>,
    pub skipped: Vec<SkippedFile>,
}

/// Copies picked files into a project in order; files past the limits are skipped with
/// a reason.
pub fn add_files(s: &AppState, project_id: Id, paths: &[PathBuf]) -> IpcResult<AddedFiles> {
    let root = files_root(s)?;
    let mut out = AddedFiles {
        cancelled: false,
        added: vec![],
        skipped: vec![],
    };
    for p in paths {
        // Hold the lock per file, not for the whole batch: copying can take a while.
        let r = import(&*s.db()?, &root, project_id, p);
        match r {
            Ok(f) => out.added.push(f),
            Err(ImportError::Store(dn_store::StoreError::NotFound(w))) => {
                return Err(IpcError::new("not_found", w));
            }
            Err(e) => {
                if let ImportError::Unreadable(io) = &e {
                    eprintln!("could not add a project file: {io}");
                }
                let reason = match e {
                    ImportError::Store(dn_store::StoreError::Invalid(w)) => w,
                    ImportError::Store(_) => "It couldn't be saved.".into(),
                    e => e.to_string(),
                };
                out.skipped.push(SkippedFile {
                    name: display_name(p),
                    reason,
                });
            }
        }
    }
    if !out.added.is_empty() {
        files_changed(s, project_id);
    }
    Ok(out)
}

/// Opens the system file picker (from Rust; the webview never chooses a path) and copies
/// the picked files into the project.
#[tauri::command]
pub async fn add_project_files(
    app: tauri::AppHandle,
    window: tauri::Window,
    state: AppStateRef<'_>,
    project_id: Id,
) -> IpcResult<AddedFiles> {
    use tauri_plugin_dialog::DialogExt;
    // Fail before the dialog if the project is gone.
    crate::commands::blocking(&state, move |s| {
        project::get_project(&*s.db()?, project_id)
            .map(|_| ())
            .map_err(Into::into)
    })
    .await?;
    let (tx, rx) = tokio::sync::oneshot::channel();
    app.dialog()
        .file()
        .set_title("Add files to the project")
        .set_parent(&window)
        .pick_files(move |picked| {
            let _ = tx.send(picked);
        });
    let picked = rx.await.map_err(|_| {
        IpcError::new(
            "internal",
            "The file picker closed unexpectedly. Try again.",
        )
    })?;
    // `None`: the user closed the picker without choosing.
    let Some(picked) = picked else {
        return Ok(AddedFiles {
            cancelled: true,
            added: vec![],
            skipped: vec![],
        });
    };
    let mut paths = Vec::with_capacity(picked.len());
    for p in picked {
        // Desktop pickers return paths; anything else is refused, not skipped silently.
        let path = p.into_path().map_err(|_| {
            IpcError::invalid("One of the chosen items isn't a file on this computer.")
        })?;
        paths.push(path);
    }
    if paths.is_empty() {
        return Ok(AddedFiles {
            cancelled: true,
            added: vec![],
            skipped: vec![],
        });
    }
    crate::commands::blocking(&state, move |s| add_files(s, project_id, &paths)).await
}

/// Soft delete; Undo restores it until the next start.
#[tauri::command]
pub async fn remove_project_file(
    state: AppStateRef<'_>,
    project_id: Id,
    file_id: Id,
) -> IpcResult<()> {
    crate::commands::blocking(&state, move |s| {
        project::delete_file(&*s.db()?, project_id, file_id)?;
        files_changed(s, project_id);
        Ok(())
    })
    .await
}

#[tauri::command]
pub async fn restore_project_file(
    state: AppStateRef<'_>,
    project_id: Id,
    file_id: Id,
) -> IpcResult<()> {
    crate::commands::blocking(&state, move |s| {
        project::restore_file(&*s.db()?, project_id, file_id)?;
        files_changed(s, project_id);
        Ok(())
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TempDir(PathBuf);
    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    fn temp() -> TempDir {
        let d = std::env::temp_dir().join(format!("dn-projects-{}", Id::new()));
        std::fs::create_dir_all(&d).unwrap();
        TempDir(d)
    }
    fn db() -> Connection {
        dn_store::open_in_memory(&crate::state::migration_sets()).unwrap()
    }

    #[test]
    fn imports_a_copy_with_its_hash_and_keeps_the_original() {
        let t = temp();
        let c = db();
        let p = project::create_project(&c, "p").unwrap();
        let src = t.0.join("notes \u{202e}txt.md");
        std::fs::write(&src, b"abc").unwrap();
        let root = t.0.join("files");
        let f = import(&c, &root, p.id, &src).unwrap();
        assert_eq!(f.size, 3);
        assert_eq!(
            f.sha256,
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(
            f.name, "notes txt.md",
            "bidi controls are stripped from the name"
        );
        assert_eq!(f.media_type, "text/markdown");
        let copy = project::file_path(&root, &f);
        assert_eq!(std::fs::read(&copy).unwrap(), b"abc");
        assert!(src.exists(), "the original stays where it was");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&copy).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        // Changing the original doesn't change the copy.
        std::fs::write(&src, b"changed").unwrap();
        assert_eq!(std::fs::read(&copy).unwrap(), b"abc");
    }

    #[test]
    fn refuses_folders_missing_files_and_large_files() {
        let t = temp();
        let c = db();
        let p = project::create_project(&c, "p").unwrap();
        let root = t.0.join("files");
        assert!(matches!(
            import(&c, &root, p.id, &t.0),
            Err(ImportError::NotAFile | ImportError::Unreadable(_))
        ));
        assert!(matches!(
            import(&c, &root, p.id, &t.0.join("missing")),
            Err(ImportError::Unreadable(_))
        ));
        let big = t.0.join("big.bin");
        let f = std::fs::File::create(&big).unwrap();
        f.set_len(MAX_FILE_BYTES + 1).unwrap();
        assert!(matches!(
            import(&c, &root, p.id, &big),
            Err(ImportError::TooLarge)
        ));
        assert!(project::list_files(&c, p.id).unwrap().is_empty());
        // Nothing half-written is left behind.
        let left: Vec<_> = std::fs::read_dir(project::project_dir(&root, p.id))
            .map(|d| d.flatten().collect())
            .unwrap_or_default();
        assert!(left.is_empty());
    }

    #[test]
    fn a_missing_project_gets_no_copy() {
        let t = temp();
        let c = db();
        let src = t.0.join("a.txt");
        std::fs::write(&src, b"a").unwrap();
        let root = t.0.join("files");
        assert!(matches!(
            import(&c, &root, Id::new(), &src),
            Err(ImportError::Store(dn_store::StoreError::NotFound(_)))
        ));
        assert!(!root.exists());
    }

    #[test]
    fn purge_deletes_copies_and_sweeps_strays() {
        let t = temp();
        let c = db();
        let root = t.0.join("files");
        let src = t.0.join("a.txt");
        std::fs::write(&src, b"a").unwrap();
        let p = project::create_project(&c, "p").unwrap();
        let keep = project::create_project(&c, "keep").unwrap();
        let fp = import(&c, &root, p.id, &src).unwrap();
        let removed = import(&c, &root, keep.id, &src).unwrap();
        let kept = import(&c, &root, keep.id, &src).unwrap();
        project::delete_file(&c, keep.id, removed.id).unwrap();
        project::delete_project(&c, p.id).unwrap();
        // Strays: a crash leftover, a copy with no row, a folder of an unknown project.
        let kdir = project::project_dir(&root, keep.id);
        std::fs::write(kdir.join(format!("{}.part", Id::new())), b"x").unwrap();
        std::fs::write(kdir.join(Id::new().to_string()), b"x").unwrap();
        std::fs::create_dir_all(root.join(Id::new().to_string())).unwrap();
        std::fs::write(root.join("junk"), b"x").unwrap();

        store::purge_deleted(&c, dn_store::now_ms()).unwrap();
        purge_on_start(&c, &root).unwrap();

        assert!(!project::file_path(&root, &fp).exists());
        assert!(!project::project_dir(&root, p.id).exists());
        assert!(!project::file_path(&root, &removed).exists());
        assert!(project::file_path(&root, &kept).exists());
        let left: Vec<_> = std::fs::read_dir(&root)
            .unwrap()
            .flatten()
            .map(|e| e.path())
            .collect();
        assert_eq!(left, vec![kdir.clone()]);
        assert_eq!(std::fs::read_dir(&kdir).unwrap().count(), 1);
    }

    #[test]
    fn a_soft_deleted_file_keeps_its_copy_for_undo() {
        let t = temp();
        let c = db();
        let root = t.0.join("files");
        let src = t.0.join("a.txt");
        std::fs::write(&src, b"a").unwrap();
        let p = project::create_project(&c, "p").unwrap();
        let f = import(&c, &root, p.id, &src).unwrap();
        project::delete_file(&c, p.id, f.id).unwrap();
        sweep(&c, &root).unwrap();
        assert!(project::file_path(&root, &f).exists());
    }

    #[test]
    fn instructions_go_only_to_chats_of_the_project() {
        let c = db();
        let p = project::create_project(&c, "Thesis").unwrap();
        project::update_project(&c, p.id, "Thesis", "Cite sources.").unwrap();
        let inside = store::create_conversation_in(&c, "q", Some(p.id)).unwrap();
        let outside = store::create_conversation(&c, "q").unwrap();
        let empty = project::create_project(&c, "Empty").unwrap();
        let in_empty = store::create_conversation_in(&c, "q", Some(empty.id)).unwrap();
        let text = instructions_for(&c, inside.id).unwrap().unwrap();
        assert!(text.contains("\"Thesis\"") && text.ends_with("Cite sources."));
        assert_eq!(instructions_for(&c, outside.id).unwrap(), None);
        assert_eq!(instructions_for(&c, in_empty.id).unwrap(), None);
    }

    #[test]
    fn instructions_follow_a_chat_that_moves() {
        let c = db();
        let a = project::create_project(&c, "A").unwrap();
        project::update_project(&c, a.id, "A", "Answer in French.").unwrap();
        let b = project::create_project(&c, "B").unwrap();
        project::update_project(&c, b.id, "B", "Answer in Dutch.").unwrap();
        let chat = store::create_conversation(&c, "q").unwrap();
        assert_eq!(instructions_for(&c, chat.id).unwrap(), None);
        store::set_conversation_project(&c, chat.id, Some(a.id)).unwrap();
        assert!(
            instructions_for(&c, chat.id)
                .unwrap()
                .unwrap()
                .contains("French")
        );
        store::set_conversation_project(&c, chat.id, Some(b.id)).unwrap();
        assert!(
            instructions_for(&c, chat.id)
                .unwrap()
                .unwrap()
                .contains("Dutch")
        );
        store::set_conversation_project(&c, chat.id, None).unwrap();
        assert_eq!(instructions_for(&c, chat.id).unwrap(), None);
    }

    #[test]
    fn media_types_come_from_the_extension() {
        assert_eq!(media_type("A.PDF"), "application/pdf");
        assert_eq!(media_type("x.rs"), "text/plain");
        assert_eq!(media_type("noext"), "application/octet-stream");
    }
}
