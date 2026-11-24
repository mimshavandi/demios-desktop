//! Resumable, hash-verified downloads.
//!
//! Bytes go to `<file>.part`; an interrupted download resumes with an HTTP `Range` request
//! (the existing bytes are re-hashed first). The file is renamed into place only after its
//! size and SHA-256 match. A `<file>.sha256` marker records a verified file so later calls
//! skip re-hashing.

use crate::catalog::CatalogEntry;
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

pub use reqwest::Client;

#[derive(Debug, thiserror::Error)]
pub enum DownloadError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("http: {0}")]
    Http(String),
    #[error("server returned {0}")]
    Status(u16),
    #[error("size mismatch: expected {expected} bytes, got {actual}")]
    Size { expected: u64, actual: u64 },
    #[error("sha256 mismatch: expected {expected}, got {actual}")]
    Hash { expected: String, actual: String },
}

/// Progress callback: (bytes done, bytes total).
pub type Progress<'a> = &'a mut (dyn FnMut(u64, u64) + Send);

pub struct Expected<'a> {
    pub url: &'a str,
    pub size: u64,
    pub sha256: &'a str,
}

fn hex(d: &[u8]) -> String {
    d.iter().map(|b| format!("{b:02x}")).collect()
}

fn marker(path: &Path) -> PathBuf {
    let mut p = path.as_os_str().to_owned();
    p.push(".sha256");
    PathBuf::from(p)
}

fn part(path: &Path) -> PathBuf {
    let mut p = path.as_os_str().to_owned();
    p.push(".part");
    PathBuf::from(p)
}

async fn hash_file(path: &Path, h: &mut Sha256) -> std::io::Result<u64> {
    let mut f = tokio::fs::File::open(path).await?;
    let mut buf = vec![0u8; 1 << 20];
    let mut n = 0;
    loop {
        let k = f.read(&mut buf).await?;
        if k == 0 {
            return Ok(n);
        }
        h.update(&buf[..k]);
        n += k as u64;
    }
}

/// True if `path` exists with the expected size and a matching verified marker.
pub async fn is_verified(path: &Path, size: u64, sha256: &str) -> bool {
    let Ok(meta) = tokio::fs::metadata(path).await else {
        return false;
    };
    if meta.len() != size {
        return false;
    }
    matches!(tokio::fs::read_to_string(marker(path)).await, Ok(m) if m.trim().eq_ignore_ascii_case(sha256))
}

const RETRIES: u32 = 5;

/// One HTTP request, appending to `part` from byte `have`.
async fn fetch(
    client: &reqwest::Client,
    exp: &Expected<'_>,
    part: &Path,
    hasher: &mut Sha256,
    have: &mut u64,
    progress: Progress<'_>,
) -> Result<(), DownloadError> {
    let mut req = client.get(exp.url);
    if *have > 0 {
        req = req.header(reqwest::header::RANGE, format!("bytes={have}-"));
    }
    let mut resp = req
        .send()
        .await
        .map_err(|e| DownloadError::Http(e.to_string()))?;
    let append = match resp.status().as_u16() {
        206 => true,
        200 => {
            if *have > 0 {
                // Server ignored the range: start over.
                *hasher = Sha256::new();
                *have = 0;
            }
            false
        }
        s => return Err(DownloadError::Status(s)),
    };
    let mut f = tokio::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .append(append)
        .truncate(!append)
        .open(part)
        .await?;
    progress(*have, exp.size);
    let result = async {
        while let Some(chunk) = resp
            .chunk()
            .await
            .map_err(|e| DownloadError::Http(e.to_string()))?
        {
            if *have + chunk.len() as u64 > exp.size {
                return Err(DownloadError::Size {
                    expected: exp.size,
                    actual: *have + chunk.len() as u64,
                });
            }
            f.write_all(&chunk).await?;
            hasher.update(&chunk);
            *have += chunk.len() as u64;
            progress(*have, exp.size);
        }
        Ok(())
    }
    .await;
    f.flush().await?;
    f.sync_all().await?;
    result?;
    if *have < exp.size {
        return Err(DownloadError::Http("connection closed early".into()));
    }
    Ok(())
}

/// Download `exp.url` to `dest`, resuming a previous partial download and retrying
/// transient network errors.
pub async fn download_to(
    client: &reqwest::Client,
    exp: &Expected<'_>,
    dest: &Path,
    progress: Progress<'_>,
) -> Result<PathBuf, DownloadError> {
    if is_verified(dest, exp.size, exp.sha256).await {
        progress(exp.size, exp.size);
        return Ok(dest.to_path_buf());
    }
    if let Some(dir) = dest.parent() {
        tokio::fs::create_dir_all(dir).await?;
    }
    let part = part(dest);
    let mut hasher = Sha256::new();
    let mut have: u64 = match tokio::fs::metadata(&part).await {
        Ok(m) if m.len() <= exp.size => hash_file(&part, &mut hasher).await?,
        Ok(_) => {
            tokio::fs::remove_file(&part).await?;
            0
        }
        Err(_) => 0,
    };
    let mut attempt = 0;
    while have < exp.size {
        match fetch(client, exp, &part, &mut hasher, &mut have, &mut *progress).await {
            Ok(()) => break,
            // Transient network errors: resume from what we have, with backoff.
            Err(DownloadError::Http(e)) if attempt < RETRIES => {
                attempt += 1;
                eprintln!("dn-models: download interrupted ({e}); retry {attempt}/{RETRIES}");
                tokio::time::sleep(std::time::Duration::from_millis(500 << attempt)).await;
            }
            Err(e) => return Err(e),
        }
    }
    if have != exp.size {
        return Err(DownloadError::Size {
            expected: exp.size,
            actual: have,
        });
    }
    let actual = hex(&hasher.finalize());
    if !actual.eq_ignore_ascii_case(exp.sha256) {
        tokio::fs::remove_file(&part).await?;
        return Err(DownloadError::Hash {
            expected: exp.sha256.to_string(),
            actual,
        });
    }
    tokio::fs::rename(&part, dest).await?;
    tokio::fs::write(marker(dest), &actual).await?;
    Ok(dest.to_path_buf())
}

/// Download a catalogue entry into `models_dir/<file>`.
pub async fn pull(
    client: &reqwest::Client,
    entry: &CatalogEntry,
    models_dir: &Path,
    progress: Progress<'_>,
) -> Result<PathBuf, DownloadError> {
    let url = entry.url();
    download_to(
        client,
        &Expected {
            url: &url,
            size: entry.size,
            sha256: &entry.sha256,
        },
        &models_dir.join(&entry.file),
        progress,
    )
    .await
}

/// Default models directory: `$DN_MODELS_DIR`, else the platform data dir + `demios/models`.
pub fn default_models_dir() -> PathBuf {
    if let Some(d) = std::env::var_os("DN_MODELS_DIR") {
        return PathBuf::from(d);
    }
    let home = || {
        std::env::var_os("HOME")
            .or_else(|| std::env::var_os("USERPROFILE"))
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("."))
    };
    let base = if cfg!(target_os = "macos") {
        home().join("Library/Application Support")
    } else if cfg!(windows) {
        std::env::var_os("LOCALAPPDATA")
            .map(PathBuf::from)
            .unwrap_or_else(home)
    } else {
        std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home().join(".local/share"))
    };
    base.join("demios").join("models")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    fn body() -> Vec<u8> {
        (0..200_000u32).map(|i| (i * 7 % 251) as u8).collect()
    }

    fn sha(b: &[u8]) -> String {
        hex(&Sha256::digest(b))
    }

    /// Serves `data`, honouring `Range: bytes=N-` when `ranges` is true.
    async fn server(data: Vec<u8>, ranges: bool) -> (String, Arc<AtomicUsize>) {
        server_with(data, ranges, false).await
    }

    /// `cut_first`: the first response promises the full body but closes halfway.
    async fn server_with(
        data: Vec<u8>,
        ranges: bool,
        cut_first: bool,
    ) -> (String, Arc<AtomicUsize>) {
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/m.gguf", l.local_addr().unwrap());
        let hits = Arc::new(AtomicUsize::new(0));
        let h = hits.clone();
        tokio::spawn(async move {
            loop {
                let (mut s, _) = l.accept().await.unwrap();
                let nth = h.fetch_add(1, Ordering::SeqCst);
                let mut buf = vec![0u8; 4096];
                let n = s.read(&mut buf).await.unwrap_or(0);
                let req = String::from_utf8_lossy(&buf[..n]).to_lowercase();
                let start = req
                    .lines()
                    .find_map(|l| l.strip_prefix("range: bytes="))
                    .and_then(|r| r.trim_end_matches('-').trim().parse::<usize>().ok())
                    .filter(|_| ranges);
                let (status, slice) = match start {
                    Some(st) => ("206 Partial Content", &data[st..]),
                    None => ("200 OK", &data[..]),
                };
                let head = format!(
                    "HTTP/1.1 {status}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                    slice.len()
                );
                let _ = s.write_all(head.as_bytes()).await;
                let slice = if cut_first && nth == 0 {
                    &slice[..slice.len() / 2]
                } else {
                    slice
                };
                let _ = s.write_all(slice).await;
                let _ = s.shutdown().await;
            }
        });
        (url, hits)
    }

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("dn-models-test-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[tokio::test]
    async fn fresh_download_verifies_and_marks() {
        let data = body();
        let (url, hits) = server(data.clone(), true).await;
        let dest = tmp("fresh").join("m.gguf");
        let exp = Expected {
            url: &url,
            size: data.len() as u64,
            sha256: &sha(&data),
        };
        let c = reqwest::Client::new();
        let mut last = 0;
        download_to(&c, &exp, &dest, &mut |d, _| last = d)
            .await
            .unwrap();
        assert_eq!(std::fs::read(&dest).unwrap(), data);
        assert_eq!(last, data.len() as u64);
        assert!(is_verified(&dest, exp.size, exp.sha256).await);
        // Second call: no request at all.
        download_to(&c, &exp, &dest, &mut |_, _| {}).await.unwrap();
        assert_eq!(hits.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn resumes_from_part_file() {
        let data = body();
        let (url, _) = server(data.clone(), true).await;
        let dest = tmp("resume").join("m.gguf");
        std::fs::write(part(&dest), &data[..77_777]).unwrap();
        let mut first = None;
        download_to(
            &reqwest::Client::new(),
            &Expected {
                url: &url,
                size: data.len() as u64,
                sha256: &sha(&data),
            },
            &dest,
            &mut |d, _| {
                first.get_or_insert(d);
            },
        )
        .await
        .unwrap();
        assert_eq!(first, Some(77_777));
        assert_eq!(std::fs::read(&dest).unwrap(), data);
    }

    #[tokio::test]
    async fn restarts_when_server_ignores_range() {
        let data = body();
        let (url, _) = server(data.clone(), false).await;
        let dest = tmp("norange").join("m.gguf");
        std::fs::write(part(&dest), &data[..1000]).unwrap();
        download_to(
            &reqwest::Client::new(),
            &Expected {
                url: &url,
                size: data.len() as u64,
                sha256: &sha(&data),
            },
            &dest,
            &mut |_, _| {},
        )
        .await
        .unwrap();
        assert_eq!(std::fs::read(&dest).unwrap(), data);
    }

    #[tokio::test]
    async fn retries_and_resumes_after_dropped_connection() {
        let data = body();
        let (url, hits) = server_with(data.clone(), true, true).await;
        let dest = tmp("dropped").join("m.gguf");
        download_to(
            &reqwest::Client::new(),
            &Expected {
                url: &url,
                size: data.len() as u64,
                sha256: &sha(&data),
            },
            &dest,
            &mut |_, _| {},
        )
        .await
        .unwrap();
        assert_eq!(std::fs::read(&dest).unwrap(), data);
        assert_eq!(hits.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn hash_mismatch_is_rejected_and_part_removed() {
        let data = body();
        let (url, _) = server(data.clone(), true).await;
        let dest = tmp("badhash").join("m.gguf");
        let e = download_to(
            &reqwest::Client::new(),
            &Expected {
                url: &url,
                size: data.len() as u64,
                sha256: &"0".repeat(64),
            },
            &dest,
            &mut |_, _| {},
        )
        .await
        .unwrap_err();
        assert!(matches!(e, DownloadError::Hash { .. }));
        assert!(!dest.exists() && !part(&dest).exists());
    }
}
