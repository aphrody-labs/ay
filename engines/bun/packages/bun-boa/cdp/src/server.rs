//! The DevTools endpoint: `/json/version`, `/json/list` over HTTP, the protocol over WebSocket.
//! Connections are accepted on their own threads; every WebSocket is handed to the browser loop,
//! which owns all pages (Boa contexts are single-threaded).

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::json;
use tungstenite::WebSocket;

use crate::net::USER_AGENT;

pub type Socket = WebSocket<TcpStream>;

/// What `/json/list` shows; refreshed by the browser loop.
#[derive(Clone)]
pub struct TargetSummary {
    pub id: String,
    pub title: String,
    pub url: String,
}

pub struct Endpoint {
    pub addr: SocketAddr,
    pub browser_id: String,
    pub targets: Arc<Mutex<Vec<TargetSummary>>>,
}

impl Endpoint {
    pub fn browser_url(&self) -> String {
        format!("ws://{}/devtools/browser/{}", self.addr, self.browser_id)
    }

    fn version(&self) -> serde_json::Value {
        json!({
            "Browser": concat!("Kitesurf/", env!("CARGO_PKG_VERSION")),
            "Protocol-Version": "1.3",
            "User-Agent": USER_AGENT,
            "V8-Version": "0.0",
            "WebKit-Version": "0.0",
            "webSocketDebuggerUrl": self.browser_url(),
        })
    }

    fn list(&self) -> serde_json::Value {
        let targets = self.targets.lock().unwrap();
        serde_json::Value::Array(
            targets
                .iter()
                .map(|t| {
                    json!({
                        "description": "",
                        "devtoolsFrontendUrl": "",
                        "id": t.id,
                        "title": t.title,
                        "type": "page",
                        "url": t.url,
                        "webSocketDebuggerUrl": format!("ws://{}/devtools/page/{}", self.addr, t.id),
                    })
                })
                .collect(),
        )
    }
}

/// A WebSocket and the path it was opened on (`/devtools/page/<id>` attaches to that page).
pub struct Incoming {
    pub socket: Socket,
    pub path: String,
}

pub fn listen(listener: TcpListener, endpoint: Arc<Endpoint>, tx: Sender<Incoming>) {
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(stream) = stream else { continue };
            let endpoint = Arc::clone(&endpoint);
            let tx = tx.clone();
            std::thread::spawn(move || {
                if let Err(error) = serve(stream, &endpoint, &tx) {
                    log::debug!("connection: {error}");
                }
            });
        }
    });
}

fn serve(mut stream: TcpStream, endpoint: &Endpoint, tx: &Sender<Incoming>) -> std::io::Result<()> {
    stream.set_nodelay(true)?;
    stream.set_read_timeout(Some(Duration::from_secs(10)))?;
    // Peek the head: a WebSocket upgrade is left unread for tungstenite's handshake.
    let mut head = [0u8; 8192];
    let mut len;
    loop {
        len = stream.peek(&mut head)?;
        if len == 0 {
            return Ok(());
        }
        if head[..len].windows(4).any(|w| w == b"\r\n\r\n") || len == head.len() {
            break;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    let text = String::from_utf8_lossy(&head[..len]).into_owned();
    let mut lines = text.lines();
    let request_line = lines.next().unwrap_or_default();
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or_default().to_string();
    let target = parts.next().unwrap_or("/").to_string();
    let upgrade = lines.any(|l| {
        let l = l.to_ascii_lowercase();
        l.starts_with("upgrade:") && l.contains("websocket")
    });

    if upgrade {
        let socket = tungstenite::accept(stream).map_err(|e| std::io::Error::other(e.to_string()))?;
        socket.get_ref().set_read_timeout(Some(Duration::from_millis(1)))?;
        let _ = tx.send(Incoming { socket, path: target });
        return Ok(());
    }

    // Plain HTTP: consume the head we peeked.
    let head_len = text.find("\r\n\r\n").map(|i| i + 4).unwrap_or(len);
    let mut sink = vec![0u8; head_len];
    stream.read_exact(&mut sink)?;
    let path = target.split('?').next().unwrap_or("/").trim_end_matches('/');
    let (status, body) = match (method.as_str(), path) {
        (_, "/json/version") => ("200 OK", endpoint.version()),
        (_, "/json" | "/json/list") => ("200 OK", endpoint.list()),
        (_, "/json/protocol") => ("200 OK", json!({ "version": { "major": "1", "minor": "3" }, "domains": [] })),
        _ => ("404 Not Found", json!({ "error": format!("no such endpoint: {path}") })),
    };
    let body = body.to_string();
    write!(
        stream,
        "HTTP/1.1 {status}\r\nContent-Type: application/json; charset=UTF-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )?;
    stream.flush()
}
