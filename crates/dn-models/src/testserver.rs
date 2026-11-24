//! Tiny HTTP/1.1 server for tests: routes by exact path (+query), records request heads.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[derive(Clone)]
pub struct Reply {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
    /// Omit `Content-Length` (body delimited by closing the connection).
    pub no_length: bool,
}

impl Reply {
    pub fn json(body: &str) -> Self {
        Self {
            status: 200,
            headers: vec![("content-type".into(), "application/json".into())],
            body: body.as_bytes().to_vec(),
            no_length: false,
        }
    }

    pub fn bytes(body: Vec<u8>) -> Self {
        Self {
            status: 200,
            headers: vec![],
            body,
            no_length: false,
        }
    }

    pub fn status(status: u16) -> Self {
        Self {
            status,
            headers: vec![],
            body: vec![],
            no_length: false,
        }
    }

    pub fn without_length(mut self) -> Self {
        self.no_length = true;
        self
    }

    pub fn header(mut self, k: &str, v: &str) -> Self {
        self.headers.push((k.into(), v.into()));
        self
    }
}

pub struct Server {
    pub url: String,
    /// Lower-cased request heads (request line + headers), in arrival order.
    pub requests: Arc<Mutex<Vec<String>>>,
}

/// Serve `routes` (key: path with query, e.g. `/api/models?search=x`). Unknown paths are 404.
pub async fn serve(routes: HashMap<String, Reply>) -> Server {
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", l.local_addr().unwrap());
    let requests = Arc::new(Mutex::new(vec![]));
    let log = requests.clone();
    let routes = Arc::new(routes);
    tokio::spawn(async move {
        loop {
            let Ok((mut s, _)) = l.accept().await else {
                return;
            };
            let routes = routes.clone();
            let log = log.clone();
            tokio::spawn(async move {
                let mut buf = vec![];
                let mut chunk = [0u8; 4096];
                while !buf.windows(4).any(|w| w == b"\r\n\r\n") {
                    match s.read(&mut chunk).await {
                        Ok(0) | Err(_) => return,
                        Ok(n) => buf.extend_from_slice(&chunk[..n]),
                    }
                }
                let head = String::from_utf8_lossy(&buf).to_string();
                let target = head.split_whitespace().nth(1).unwrap_or("/").to_string();
                log.lock().unwrap().push(head.to_lowercase());
                let r = routes.get(&target).cloned().unwrap_or(Reply::status(404));
                let mut out = if r.no_length {
                    format!("HTTP/1.1 {} X\r\nconnection: close\r\n", r.status)
                } else {
                    format!(
                        "HTTP/1.1 {} X\r\ncontent-length: {}\r\nconnection: close\r\n",
                        r.status,
                        r.body.len()
                    )
                };
                for (k, v) in &r.headers {
                    out.push_str(&format!("{k}: {v}\r\n"));
                }
                out.push_str("\r\n");
                let _ = s.write_all(out.as_bytes()).await;
                let _ = s.write_all(&r.body).await;
                let _ = s.shutdown().await;
            });
        }
    });
    Server { url, requests }
}
