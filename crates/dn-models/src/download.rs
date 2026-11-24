//! Resumable, hash-verified downloads.
//!
//! Bytes go to `<file>.part`; an interrupted download resumes with an HTTP `Range` request
//! (the existing bytes are re-hashed first). The file is renamed into place only after its
//! size and SHA-256 match. A `<file>.sha256` marker records a verified file so later calls
//! skip re-hashing.
//!
//! Safety rails: [`client`] is HTTPS-only with connect and read timeouts; a `<file>.lock`
//! (OS file lock, released if the process dies) stops two pulls of the same file; free disk
//! space is checked before any byte is written; a `206` whose `Content-Range` doesn't start
//! where the partial file ends restarts the download from zero. Network errors, 429 and 5xx
//! are retried with backoff, and the retry budget resets whenever bytes arrive.

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
    #[error("{0} is already being downloaded by another process")]
    Busy(PathBuf),
    #[error("not enough disk space: need {need} bytes, {free} free")]
    DiskFull { need: u64, free: u64 },
}

impl DownloadError {
    fn retryable(&self) -> bool {
        match self {
            Self::Http(_) => true,
            Self::Status(s) => *s == 429 || *s >= 500,
            _ => false,
        }
    }
}

/// The client downloads should use: HTTPS only, 15 s connect timeout, 60 s read timeout
/// (a stalled transfer fails and is resumed instead of hanging).
pub fn client() -> Result<Client, DownloadError> {
    Client::builder()
        .https_only(true)
        .connect_timeout(std::time::Duration::from_secs(15))
        .read_timeout(std::time::Duration::from_secs(60))
        .build()
        .map_err(|e| DownloadError::Http(e.to_string()))
}

/// Progress callback: (bytes done, bytes total).
pub type Progress<'a> = &'a mut (dyn FnMut(u64, u64) + Send);

pub struct Expected<'a> {
    pub url: &'a str,
    pub size: u64,
    pub sha256: &'a str,
    /// `Authorization: Bearer` for gated repos. reqwest drops it on redirects to another
    /// host (the Hub redirects file downloads to its CDN).
    pub bearer: Option<&'a str>,
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
    with_suffix(path, ".part")
}

fn with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut p = path.as_os_str().to_owned();
    p.push(suffix);
    PathBuf::from(p)
}

/// Exclusive lock on `<dest>.lock`, held for the whole download.
fn lock(dest: &Path) -> Result<std::fs::File, DownloadError> {
    let f = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(with_suffix(dest, ".lock"))?;
    match f.try_lock() {
        Ok(()) => Ok(f),
        Err(std::fs::TryLockError::WouldBlock) => Err(DownloadError::Busy(dest.to_path_buf())),
        Err(std::fs::TryLockError::Error(e)) => Err(e.into()),
    }
}

/// Free bytes on the volume holding `dir` (`None` if it can't be told).
fn free_space(dir: &Path) -> Option<u64> {
    let dir = strip_verbatim(std::fs::canonicalize(dir).ok()?);
    let disks = sysinfo::Disks::new_with_refreshed_list();
    disks
        .list()
        .iter()
        .filter(|d| dir.starts_with(d.mount_point()))
        .max_by_key(|d| d.mount_point().as_os_str().len())
        .map(|d| d.available_space())
}

/// Windows `canonicalize` returns verbatim paths (`\\?\C:\…`, `\\?\UNC\srv\share\…`) that
/// never start with a mount point like `C:\`; turn them back into ordinary paths.
fn strip_verbatim(p: PathBuf) -> PathBuf {
    let Some(s) = p.to_str() else { return p };
    if let Some(rest) = s.strip_prefix(r"\\?\UNC\") {
        return PathBuf::from(format!(r"\\{rest}"));
    }
    match s.strip_prefix(r"\\?\") {
        Some(rest) if rest.as_bytes().get(1) == Some(&b':') => PathBuf::from(rest),
        _ => p,
    }
}

/// Start offset of a `Content-Range: bytes START-END/TOTAL` header.
fn range_start(resp: &reqwest::Response) -> Option<u64> {
    let v = resp
        .headers()
        .get(reqwest::header::CONTENT_RANGE)?
        .to_str()
        .ok()?;
    v.trim()
        .strip_prefix("bytes ")?
        .split('-')
        .next()?
        .trim()
        .parse()
        .ok()
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
    if let Some(t) = exp.bearer {
        req = req.bearer_auth(t);
    }
    if *have > 0 {
        req = req.header(reqwest::header::RANGE, format!("bytes={have}-"));
    }
    let mut resp = req
        .send()
        .await
        .map_err(|e| DownloadError::Http(e.to_string()))?;
    let append = match resp.status().as_u16() {
        206 if range_start(&resp) == Some(*have) => true,
        206 => {
            // The server answered a different range than we asked for: throw the partial
            // file away and start again from byte 0 on the next attempt.
            *hasher = Sha256::new();
            *have = 0;
            tokio::fs::write(part, b"").await?;
            return Err(DownloadError::Http(
                "unexpected Content-Range; restarting".into(),
            ));
        }
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
    let held = lock(dest)?;
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
    let dir = dest.parent().unwrap_or(Path::new("."));
    if let Some(free) = free_space(dir) {
        let need = exp.size - have;
        if need > free {
            return Err(DownloadError::DiskFull { need, free });
        }
    }
    let mut attempt = 0;
    while have < exp.size {
        let before = have;
        match fetch(client, exp, &part, &mut hasher, &mut have, &mut *progress).await {
            Ok(()) => break,
            // Transient errors: resume from what we have, with backoff. Progress since the
            // last failure earns a fresh retry budget.
            Err(e) if e.retryable() => {
                if have > before {
                    attempt = 0;
                }
                if attempt >= RETRIES {
                    return Err(e);
                }
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
    // Done: release and remove the lock file (a later pull sees the verified marker first).
    drop(held);
    let _ = tokio::fs::remove_file(with_suffix(dest, ".lock")).await;
    Ok(dest.to_path_buf())
}

/// Download a catalogue entry into `models_dir/<file>`.
pub async fn pull(
    client: &reqwest::Client,
    entry: &CatalogEntry,
    models_dir: &Path,
    progress: Progress<'_>,
) -> Result<PathBuf, DownloadError> {
    if !entry.has_plain_file_name() {
        return Err(DownloadError::Io(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!(
                "catalogue file name {:?} is not a plain file name",
                entry.file
            ),
        )));
    }
    let url = entry.url();
    download_to(
        client,
        &Expected {
            url: &url,
            size: entry.size,
            sha256: &entry.sha256,
            bearer: None,
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

    #[derive(Clone, Copy, Default)]
    struct Mode {
        /// Honour `Range: bytes=N-`.
        ranges: bool,
        /// First response promises the full body but closes halfway.
        cut_first: bool,
        /// First ranged response is a 206 for the whole file (`bytes 0-…`).
        wrong_range_first: bool,
        /// First response is a 503.
        unavailable_first: bool,
    }

    /// Serves `data`, honouring `Range: bytes=N-` when `ranges` is true.
    async fn server(data: Vec<u8>, ranges: bool) -> (String, Arc<AtomicUsize>) {
        server_with(
            data,
            Mode {
                ranges,
                ..Default::default()
            },
        )
        .await
    }

    async fn server_with(data: Vec<u8>, mode: Mode) -> (String, Arc<AtomicUsize>) {
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
                    .filter(|_| mode.ranges);
                if mode.unavailable_first && nth == 0 {
                    let _ = s
                        .write_all(b"HTTP/1.1 503 Service Unavailable\r\ncontent-length: 0\r\nconnection: close\r\n\r\n")
                        .await;
                    continue;
                }
                let start = match start {
                    Some(_) if mode.wrong_range_first && nth == 0 => Some(0),
                    st => st,
                };
                let (status, slice, range) = match start {
                    Some(st) => (
                        "206 Partial Content",
                        &data[st..],
                        format!(
                            "content-range: bytes {st}-{}/{}\r\n",
                            data.len() - 1,
                            data.len()
                        ),
                    ),
                    None => ("200 OK", &data[..], String::new()),
                };
                let head = format!(
                    "HTTP/1.1 {status}\r\ncontent-length: {}\r\n{range}connection: close\r\n\r\n",
                    slice.len()
                );
                let _ = s.write_all(head.as_bytes()).await;
                let slice = if mode.cut_first && nth == 0 {
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
            bearer: None,
        };
        let c = reqwest::Client::new();
        let mut last = 0;
        download_to(&c, &exp, &dest, &mut |d, _| last = d)
            .await
            .unwrap();
        assert_eq!(std::fs::read(&dest).unwrap(), data);
        assert_eq!(last, data.len() as u64);
        assert!(is_verified(&dest, exp.size, exp.sha256).await);
        assert!(!with_suffix(&dest, ".lock").exists());
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
                bearer: None,
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
                bearer: None,
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
        let (url, hits) = server_with(
            data.clone(),
            Mode {
                ranges: true,
                cut_first: true,
                ..Default::default()
            },
        )
        .await;
        let dest = tmp("dropped").join("m.gguf");
        download_to(
            &reqwest::Client::new(),
            &Expected {
                url: &url,
                size: data.len() as u64,
                sha256: &sha(&data),
                bearer: None,
            },
            &dest,
            &mut |_, _| {},
        )
        .await
        .unwrap();
        assert_eq!(std::fs::read(&dest).unwrap(), data);
        assert_eq!(hits.load(Ordering::SeqCst), 2);
    }

    async fn get(url: &str, data: &[u8], dest: &Path) -> Result<PathBuf, DownloadError> {
        download_to(
            &reqwest::Client::new(),
            &Expected {
                url,
                size: data.len() as u64,
                sha256: &sha(data),
                bearer: None,
            },
            dest,
            &mut |_, _| {},
        )
        .await
    }

    #[tokio::test]
    async fn wrong_content_range_restarts_cleanly() {
        let data = body();
        let (url, hits) = server_with(
            data.clone(),
            Mode {
                ranges: true,
                wrong_range_first: true,
                ..Default::default()
            },
        )
        .await;
        let dest = tmp("wrongrange").join("m.gguf");
        std::fs::write(part(&dest), &data[..5000]).unwrap();
        get(&url, &data, &dest).await.unwrap();
        assert_eq!(std::fs::read(&dest).unwrap(), data);
        assert_eq!(hits.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn retries_5xx() {
        let data = body();
        let (url, hits) = server_with(
            data.clone(),
            Mode {
                ranges: true,
                unavailable_first: true,
                ..Default::default()
            },
        )
        .await;
        let dest = tmp("503").join("m.gguf");
        get(&url, &data, &dest).await.unwrap();
        assert_eq!(hits.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn second_concurrent_pull_is_busy() {
        let dest = tmp("busy").join("m.gguf");
        let _held = lock(&dest).unwrap();
        assert!(matches!(lock(&dest), Err(DownloadError::Busy(_))));
    }

    #[test]
    fn https_only_client_refuses_http() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let e = rt
            .block_on(client().unwrap().get("http://127.0.0.1:9/x").send())
            .unwrap_err();
        assert!(e.is_builder() || e.to_string().contains("https"), "{e}");
    }

    #[test]
    fn free_space_is_known_for_temp_dir() {
        assert!(free_space(&std::env::temp_dir()).is_some_and(|f| f > 0));
    }

    #[test]
    fn verbatim_prefix_stripped() {
        let f = |s: &str| {
            strip_verbatim(PathBuf::from(s))
                .to_string_lossy()
                .into_owned()
        };
        assert_eq!(f(r"\\?\C:\Users\x"), r"C:\Users\x");
        assert_eq!(f(r"\\?\UNC\srv\share\m"), r"\\srv\share\m");
        assert_eq!(f(r"\\?\Volume{abc}\m"), r"\\?\Volume{abc}\m");
        assert_eq!(f("/tmp/x"), "/tmp/x");
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
                bearer: None,
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
