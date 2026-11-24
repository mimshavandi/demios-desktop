//! Models already on disk (ADR-0011): GGUFs in the standard Hugging Face cache, and the ones
//! Demios downloaded itself.
//!
//! **Read-only.** This module only lists directories, reads metadata, symlink targets and
//! `refs/*` files. It has no delete or write API: the Hugging Face cache belongs to other
//! tools, and Demios never removes files it didn't download (those carry a
//! [`SourceManifest`]; see [`list_downloaded`]).
//!
//! Cache layout (huggingface_hub): `<cache>/models--<owner>--<name>/snapshots/<commit>/<path>`,
//! each file normally a symlink to `../../blobs/<hash>`, where `<hash>` is the file's SHA-256
//! for LFS files. Where symlinks aren't available (some Windows setups) the snapshot holds
//! plain copies and the hash is unknown.

use crate::hub::{GgufVariant, RepoFile, SourceManifest, gguf_variants, validate_repo_id};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// The Hugging Face hub cache: `$HF_HUB_CACHE`, else `$HF_HOME/hub`, else
/// `$XDG_CACHE_HOME/huggingface/hub`, else `~/.cache/huggingface/hub` (also on macOS and
/// Windows, like huggingface_hub).
pub fn hf_cache_dir() -> Option<PathBuf> {
    let env = |k: &str| {
        std::env::var_os(k)
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
    };
    if let Some(p) = env("HF_HUB_CACHE") {
        return Some(p);
    }
    if let Some(p) = env("HF_HOME") {
        return Some(p.join("hub"));
    }
    if let Some(p) = env("XDG_CACHE_HOME") {
        return Some(p.join("huggingface").join("hub"));
    }
    env("HOME")
        .or_else(|| env("USERPROFILE"))
        .map(|h| h.join(".cache").join("huggingface").join("hub"))
}

/// One GGUF variant found in the Hugging Face cache.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CachedModel {
    pub repo: String,
    /// Snapshot directory name (the commit SHA).
    pub revision: String,
    /// Branches/tags in `refs/` that point at this snapshot (e.g. `main`).
    pub refs: Vec<String>,
    /// File sizes from disk; `sha256` from the blob name when it is a 64-hex LFS hash
    /// (not re-hashed here: hash before trusting it for anything but display).
    pub variant: GgufVariant,
    /// Path to load (first part), inside the snapshot.
    pub path: PathBuf,
}

/// A model Demios downloaded from the Hub (has a source manifest).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DownloadedModel {
    pub path: PathBuf,
    pub manifest: SourceManifest,
}

const MAX_REPOS: usize = 10_000;
const MAX_FILES_PER_SNAPSHOT: usize = 10_000;

/// Every complete GGUF variant in `cache_dir`, sorted by repo then name. Missing or
/// unreadable directories are skipped, never an error.
pub fn discover(cache_dir: &Path) -> Vec<CachedModel> {
    let mut out = vec![];
    let Ok(entries) = std::fs::read_dir(cache_dir) else {
        return out;
    };
    for e in entries.flatten().take(MAX_REPOS) {
        let name = e.file_name().to_string_lossy().into_owned();
        let Some(rest) = name.strip_prefix("models--") else {
            continue;
        };
        // `models--owner--name`; names may contain `-` but not `--`.
        let Some((owner, model)) = rest.split_once("--") else {
            continue;
        };
        let repo = format!("{owner}/{model}");
        if validate_repo_id(&repo).is_err() {
            continue;
        }
        let dir = e.path();
        let refs = read_refs(&dir.join("refs"));
        let Ok(snaps) = std::fs::read_dir(dir.join("snapshots")) else {
            continue;
        };
        for s in snaps.flatten() {
            let revision = s.file_name().to_string_lossy().into_owned();
            let snap = s.path();
            let files = snapshot_files(&snap);
            for variant in gguf_variants(&files) {
                let Some(first) = variant.files.first() else {
                    continue;
                };
                let path = join_repo_path(&snap, &first.path);
                out.push(CachedModel {
                    repo: repo.clone(),
                    revision: revision.clone(),
                    refs: refs
                        .iter()
                        .filter(|(_, sha)| *sha == revision)
                        .map(|(r, _)| r.clone())
                        .collect(),
                    variant,
                    path,
                });
            }
        }
    }
    out.sort_by(|a, b| (&a.repo, &a.variant.name).cmp(&(&b.repo, &b.variant.name)));
    out
}

/// [`discover`] on [`hf_cache_dir`].
pub fn discover_default() -> Vec<CachedModel> {
    hf_cache_dir().map(|d| discover(&d)).unwrap_or_default()
}

/// Models Demios downloaded into `models_dir/hf/…`: each has `<first part>.source.json`, and
/// only complete, verified downloads are returned (every part present with its size and a
/// `.sha256` marker matching the manifest, i.e. it passed the hash check when downloaded).
pub fn list_downloaded(models_dir: &Path) -> Vec<DownloadedModel> {
    let mut out = vec![];
    let mut stack = vec![(models_dir.join("hf"), 0usize)];
    while let Some((dir, depth)) = stack.pop() {
        if depth > 8 {
            continue;
        }
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for e in entries.flatten() {
            let p = e.path();
            let Ok(ft) = e.file_type() else { continue };
            if ft.is_dir() {
                stack.push((p, depth + 1));
            } else if let Some(model) = p
                .to_str()
                .and_then(|s| s.strip_suffix(".source.json"))
                .map(PathBuf::from)
                && let Some(manifest) = SourceManifest::read(&model)
                && model.is_file()
                && verified(models_dir, &manifest)
            {
                out.push(DownloadedModel {
                    path: model,
                    manifest,
                });
            }
        }
    }
    out.sort_by(|a, b| a.path.cmp(&b.path));
    out
}

fn verified(models_dir: &Path, m: &SourceManifest) -> bool {
    !m.files.is_empty()
        && m.files.iter().all(|f| {
            let Some(sha) = f.sha256.as_deref() else {
                return false;
            };
            crate::hub::HubClient::local_path(models_dir, &m.repo, &f.path)
                .is_ok_and(|p| crate::download::is_verified_blocking(&p, f.size, sha))
        })
}

/// `refs/<name>` files: (ref name, commit). Nested refs (`refs/pr/1`) included.
fn read_refs(dir: &Path) -> Vec<(String, String)> {
    let mut out = vec![];
    let mut stack = vec![(dir.to_path_buf(), String::new())];
    while let Some((d, prefix)) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&d) else {
            continue;
        };
        for e in entries.flatten() {
            let name = format!("{prefix}{}", e.file_name().to_string_lossy());
            match e.file_type() {
                Ok(t) if t.is_dir() && prefix.matches('/').count() < 4 => {
                    stack.push((e.path(), format!("{name}/")))
                }
                Ok(t) if t.is_file() => {
                    if let Ok(sha) = std::fs::read_to_string(e.path()) {
                        out.push((name, sha.trim().to_string()));
                    }
                }
                _ => {}
            }
        }
    }
    out
}

/// GGUF files under a snapshot, as repo-relative paths. Dangling links (download still
/// running or blob removed) are skipped.
fn snapshot_files(snap: &Path) -> Vec<RepoFile> {
    let mut out = vec![];
    let mut stack = vec![(snap.to_path_buf(), String::new(), 0usize)];
    while let Some((d, prefix, depth)) = stack.pop() {
        if depth > 8 || out.len() >= MAX_FILES_PER_SNAPSHOT {
            continue;
        }
        let Ok(entries) = std::fs::read_dir(&d) else {
            continue;
        };
        for e in entries.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            let rel = format!("{prefix}{name}");
            let p = e.path();
            // symlink_metadata: don't recurse through directory symlinks.
            let Ok(lm) = std::fs::symlink_metadata(&p) else {
                continue;
            };
            if lm.is_dir() {
                stack.push((p, format!("{rel}/"), depth + 1));
                continue;
            }
            if !name.to_ascii_lowercase().ends_with(".gguf") {
                continue;
            }
            // Follows the link; fails for a dangling one.
            let Ok(m) = std::fs::metadata(&p) else {
                continue;
            };
            if !m.is_file() {
                continue;
            }
            let sha256 = if lm.file_type().is_symlink() {
                std::fs::read_link(&p)
                    .ok()
                    .and_then(|t| {
                        t.file_name()
                            .map(|n| n.to_string_lossy().to_ascii_lowercase())
                    })
                    .filter(|h| h.len() == 64 && h.chars().all(|c| c.is_ascii_hexdigit()))
            } else {
                None
            };
            out.push(RepoFile {
                path: rel,
                size: m.len(),
                sha256,
            });
        }
    }
    out
}

fn join_repo_path(base: &Path, rel: &str) -> PathBuf {
    let mut p = base.to_path_buf();
    for seg in rel.split('/') {
        p.push(seg);
    }
    p
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("dn-cache-test-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    const REV: &str = "7c41481f57cb95916b40956ab2f0b139b296d974";

    #[cfg(unix)]
    fn link(target: &Path, at: &Path) {
        std::os::unix::fs::symlink(target, at).unwrap();
    }

    /// Lay out a cache like huggingface_hub does (symlinks on Unix, copies elsewhere).
    fn fake_cache(root: &Path) -> (String, PathBuf) {
        let repo = root.join("models--Qwen--Qwen3-8B-GGUF");
        let blobs = repo.join("blobs");
        let snap = repo.join("snapshots").join(REV);
        std::fs::create_dir_all(&blobs).unwrap();
        std::fs::create_dir_all(snap.join("Q8_0")).unwrap();
        std::fs::create_dir_all(repo.join("refs")).unwrap();
        std::fs::write(repo.join("refs/main"), REV).unwrap();
        let hash = "d98cdcbd03e17ce47681435b5150e34c1417f50b5c0019dd560e4882c5745785".to_string();
        let files = [
            (hash.clone(), "Qwen3-8B-Q4_K_M.gguf", 100usize),
            ("a".repeat(64), "Q8_0/Qwen3-8B-Q8_0-00001-of-00002.gguf", 50),
            ("b".repeat(64), "Q8_0/Qwen3-8B-Q8_0-00002-of-00002.gguf", 60),
            ("c".repeat(64), "README.md", 3),
        ];
        for (h, rel, size) in files {
            let blob = blobs.join(&h);
            std::fs::write(&blob, vec![0u8; size]).unwrap();
            let at = snap.join(rel);
            #[cfg(unix)]
            link(&blob, &at);
            #[cfg(not(unix))]
            std::fs::copy(&blob, &at).unwrap();
        }
        // A download still running: link to a missing blob.
        #[cfg(unix)]
        link(
            &blobs.join("f".repeat(64)),
            &snap.join("Qwen3-8B-Q6_K.gguf"),
        );
        // Not a model repo.
        std::fs::create_dir_all(root.join("datasets--x--y/snapshots/1")).unwrap();
        (hash, snap)
    }

    #[test]
    fn finds_variants_in_hf_cache() {
        let root = tmp("hf");
        let (hash, snap) = fake_cache(&root);
        let before: Vec<_> = walk(&root);
        let found = discover(&root);
        let names: Vec<_> = found.iter().map(|m| m.variant.name.as_str()).collect();
        assert_eq!(names, ["Qwen3-8B-Q4_K_M", "Qwen3-8B-Q8_0"]);
        let q4 = &found[0];
        assert_eq!(q4.repo, "Qwen/Qwen3-8B-GGUF");
        assert_eq!(q4.revision, REV);
        assert_eq!(q4.refs, ["main"]);
        assert_eq!(q4.path, snap.join("Qwen3-8B-Q4_K_M.gguf"));
        assert_eq!(q4.variant.total_size, 100);
        #[cfg(unix)]
        assert_eq!(q4.variant.files[0].sha256.as_deref(), Some(hash.as_str()));
        #[cfg(not(unix))]
        let _ = hash;
        let q8 = &found[1];
        assert_eq!(q8.variant.files.len(), 2);
        assert_eq!(q8.variant.total_size, 110);
        assert_eq!(
            q8.path,
            snap.join("Q8_0").join("Qwen3-8B-Q8_0-00001-of-00002.gguf")
        );
        // Read-only: nothing created, changed or removed.
        assert_eq!(walk(&root), before);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn missing_cache_is_empty() {
        assert!(discover(Path::new("/nonexistent/dn-cache")).is_empty());
    }

    #[test]
    fn lists_only_files_with_manifests() {
        let dir = tmp("ours");
        let d = dir.join("hf/o--n");
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("m.gguf"), b"x").unwrap();
        std::fs::write(d.join("m.gguf.sha256"), "a".repeat(64)).unwrap();
        std::fs::write(d.join("other.gguf"), b"x").unwrap();
        // Downloaded but its marker doesn't match: not listed.
        std::fs::write(d.join("bad.gguf"), b"x").unwrap();
        std::fs::write(d.join("bad.gguf.sha256"), "b".repeat(64)).unwrap();
        let m = SourceManifest {
            source: "huggingface".into(),
            endpoint: "https://huggingface.co".into(),
            repo: "o/n".into(),
            revision: REV.into(),
            files: vec![RepoFile {
                path: "m.gguf".into(),
                size: 1,
                sha256: Some("a".repeat(64)),
            }],
            license: crate::hub::LicenseInfo::classify(Some("mit"), None, None),
            card_url: "https://huggingface.co/o/n".into(),
            downloaded_at: 1,
        };
        std::fs::write(
            SourceManifest::path_for(&d.join("m.gguf")),
            serde_json::to_vec(&m).unwrap(),
        )
        .unwrap();
        let mut bad = m.clone();
        bad.files[0].path = "bad.gguf".into();
        std::fs::write(
            SourceManifest::path_for(&d.join("bad.gguf")),
            serde_json::to_vec(&bad).unwrap(),
        )
        .unwrap();
        // A manifest whose model file is gone is ignored.
        std::fs::write(
            SourceManifest::path_for(&d.join("gone.gguf")),
            serde_json::to_vec(&m).unwrap(),
        )
        .unwrap();
        let l = list_downloaded(&dir);
        assert_eq!(l.len(), 1);
        assert_eq!(l[0].path, d.join("m.gguf"));
        assert_eq!(l[0].manifest.repo, "o/n");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn cache_dir_env_precedence() {
        // Pure function of the environment; only check the default shape here to avoid
        // mutating process env in parallel tests.
        if std::env::var_os("HF_HUB_CACHE").is_none()
            && std::env::var_os("HF_HOME").is_none()
            && std::env::var_os("XDG_CACHE_HOME").is_none()
        {
            let d = hf_cache_dir().unwrap();
            assert!(d.ends_with(Path::new(".cache/huggingface/hub")));
        }
    }

    /// (path, len, is_symlink) of everything under `root`.
    fn walk(root: &Path) -> Vec<(PathBuf, u64, bool)> {
        let mut out = vec![];
        let mut stack = vec![root.to_path_buf()];
        while let Some(d) = stack.pop() {
            for e in std::fs::read_dir(&d).unwrap().flatten() {
                let m = std::fs::symlink_metadata(e.path()).unwrap();
                if m.is_dir() {
                    stack.push(e.path());
                }
                out.push((e.path(), m.len(), m.file_type().is_symlink()));
            }
        }
        out.sort();
        out
    }
}
