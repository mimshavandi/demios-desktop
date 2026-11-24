//! Tiny HTTP/1.1 fixture server: replays canned responses in order and records requests.
#![allow(dead_code)]

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::Notify;

#[derive(Clone, Debug)]
pub struct Recorded {
    pub method: String,
    pub path: String,
    pub headers: Vec<(String, String)>,
    pub body: serde_json::Value,
}

impl Recorded {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
}

pub struct Reply {
    pub status: u16,
    pub content_type: &'static str,
    pub body: Vec<u8>,
    /// Send the body in SSE-event-sized pieces, pausing after the second until `gate` fires.
    pub gate: Option<Arc<Notify>>,
}

impl Reply {
    pub fn sse(fixture: &str) -> Self {
        Self {
            status: 200,
            content_type: "text/event-stream",
            body: read(fixture),
            gate: None,
        }
    }

    pub fn json(status: u16, fixture: &str) -> Self {
        Self {
            status,
            content_type: "application/json",
            body: read(fixture),
            gate: None,
        }
    }
}

pub fn read(fixture: &str) -> Vec<u8> {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(fixture);
    std::fs::read(&p).unwrap_or_else(|e| panic!("{}: {e}", p.display()))
}

pub struct Server {
    pub base: String,
    pub requests: Arc<Mutex<Vec<Recorded>>>,
}

impl Server {
    pub fn requests(&self) -> Vec<Recorded> {
        self.requests.lock().unwrap().clone()
    }
}

pub async fn serve(replies: Vec<Reply>) -> Server {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let requests = Arc::new(Mutex::new(Vec::new()));
    let rec = requests.clone();
    tokio::spawn(async move {
        for reply in replies {
            let (mut sock, _) = listener.accept().await.unwrap();
            let mut buf = Vec::new();
            let mut tmp = [0u8; 4096];
            let header_end = loop {
                let n = sock.read(&mut tmp).await.unwrap();
                assert!(n > 0, "client closed before sending headers");
                buf.extend_from_slice(&tmp[..n]);
                if let Some(p) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                    break p + 4;
                }
            };
            let head = String::from_utf8_lossy(&buf[..header_end]).to_string();
            let mut lines = head.split("\r\n");
            let mut start = lines.next().unwrap().split(' ');
            let method = start.next().unwrap().to_string();
            let path = start.next().unwrap().to_string();
            let headers: Vec<(String, String)> = lines
                .filter_map(|l| l.split_once(':'))
                .map(|(k, v)| (k.trim().to_ascii_lowercase(), v.trim().to_string()))
                .collect();
            let len: usize = headers
                .iter()
                .find(|(k, _)| k == "content-length")
                .map(|(_, v)| v.parse().unwrap())
                .unwrap_or(0);
            while buf.len() < header_end + len {
                let n = sock.read(&mut tmp).await.unwrap();
                buf.extend_from_slice(&tmp[..n]);
            }
            let body_bytes = &buf[header_end..header_end + len];
            let body = if body_bytes.is_empty() {
                serde_json::Value::Null
            } else {
                serde_json::from_slice(body_bytes).unwrap()
            };
            rec.lock().unwrap().push(Recorded {
                method,
                path,
                headers,
                body,
            });
            let head = format!(
                "HTTP/1.1 {} X\r\ncontent-type: {}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                reply.status,
                reply.content_type,
                reply.body.len()
            );
            sock.write_all(head.as_bytes()).await.unwrap();
            // Write event by event so the client must parse incrementally.
            let text = reply.body;
            let mut pieces: Vec<&[u8]> = Vec::new();
            let mut rest = &text[..];
            while let Some(p) = rest.windows(2).position(|w| w == b"\n\n") {
                pieces.push(&rest[..p + 2]);
                rest = &rest[p + 2..];
            }
            pieces.push(rest);
            for (i, piece) in pieces.iter().enumerate() {
                if sock.write_all(piece).await.is_err() {
                    break; // client hung up (cancellation tests)
                }
                let _ = sock.flush().await;
                if i == 1
                    && let Some(g) = &reply.gate
                {
                    g.notified().await;
                }
            }
            let _ = sock.shutdown().await;
        }
    });
    Server {
        base: format!("http://{addr}"),
        requests,
    }
}
