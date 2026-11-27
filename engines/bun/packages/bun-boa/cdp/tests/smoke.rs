//! Starts the binary, speaks CDP to it over a WebSocket like agent-browser does, runs a page whose
//! script mutates the DOM, reads the mutation back and screenshots it.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use base64::Engine;
use serde_json::{Value, json};
use tungstenite::stream::MaybeTlsStream;
use tungstenite::{Message, WebSocket};

struct Server(Child);

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

struct Client {
    socket: WebSocket<MaybeTlsStream<TcpStream>>,
    next: u64,
    events: Vec<Value>,
}

impl Client {
    fn call(&mut self, session: Option<&str>, method: &str, params: Value) -> Value {
        self.next += 1;
        let id = self.next;
        let mut message = json!({ "id": id, "method": method, "params": params });
        if let Some(session) = session {
            message["sessionId"] = session.into();
        }
        self.socket.send(Message::text(message.to_string())).unwrap();
        loop {
            let message = self.read();
            if message["id"].as_u64() == Some(id) {
                if let Some(error) = message.get("error") {
                    panic!("{method} failed: {error}");
                }
                return message["result"].clone();
            }
            self.events.push(message);
        }
    }

    fn read(&mut self) -> Value {
        match self.socket.read().expect("websocket read") {
            Message::Text(text) => serde_json::from_str(&text).unwrap(),
            other => panic!("unexpected message {other:?}"),
        }
    }

    fn wait_event(&mut self, method: &str) -> Value {
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            if let Some(i) = self.events.iter().position(|e| e["method"] == method) {
                return self.events.remove(i);
            }
            assert!(Instant::now() < deadline, "no {method} event");
            let message = self.read();
            self.events.push(message);
        }
    }
}

fn http_get(port: u16, path: &str) -> String {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
    write!(stream, "GET {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n").unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).unwrap();
    response.split_once("\r\n\r\n").map(|(_, body)| body.to_string()).unwrap_or_default()
}

const PAGE: &str = r#"<!doctype html>
<html>
  <head><style>body { margin: 0 } #root { background: rgb(255, 0, 0); width: 200px; height: 100px }</style></head>
  <body>
    <div id="root">before</div>
    <button id="btn" onclick="this.textContent = 'pressed'" style="position: absolute; left: 300px; top: 20px; width: 80px; height: 30px">press</button>
    <input id="name" style="position: absolute; left: 300px; top: 80px; width: 120px">
    <script>
      const p = document.createElement("p");
      p.id = "added";
      p.textContent = "mutated by script";
      document.getElementById("root").appendChild(p);
      console.log("script ran");
      window.inputs = 0;
      document.getElementById("name").addEventListener("input", () => window.inputs++);
    </script>
  </body>
</html>"#;

/// Over the network (rustls with the RustCrypto provider): `cargo test -- --ignored`.
#[test]
#[ignore]
fn https_page() {
    let (_server, mut client, _) = start();
    let target = client.call(None, "Target.createTarget", json!({ "url": "about:blank" }))["targetId"].as_str().unwrap().to_string();
    let session = client.call(None, "Target.attachToTarget", json!({ "targetId": target, "flatten": true }))["sessionId"].as_str().unwrap().to_string();
    let s = Some(session.as_str());
    client.call(s, "Page.enable", json!({}));
    let navigated = client.call(s, "Page.navigate", json!({ "url": "https://example.com/" }));
    assert!(navigated.get("errorText").is_none(), "navigate: {navigated}");
    client.wait_event("Page.loadEventFired");
    let title = client.call(s, "Runtime.evaluate", json!({ "expression": "document.title", "returnByValue": true }));
    assert_eq!(title["result"]["value"], "Example Domain");
}

fn start() -> (Server, Client, String) {
    let mut child = Command::new(env!("CARGO_BIN_EXE_kitesurf-cdp"))
        .args(["serve", "--host", "127.0.0.1", "--port", "0"])
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn kitesurf-cdp");
    let mut stderr = BufReader::new(child.stderr.take().unwrap());
    let server = Server(child);
    let mut line = String::new();
    stderr.read_line(&mut line).unwrap();
    std::thread::spawn(move || {
        for line in stderr.lines().map_while(Result::ok) {
            eprintln!("[kitesurf-cdp] {line}");
        }
    });
    let ws_url = line.trim().strip_prefix("DevTools listening on ").expect("listening line").to_string();
    let (socket, _) = tungstenite::connect(ws_url.as_str()).expect("websocket connect");
    if let MaybeTlsStream::Plain(stream) = socket.get_ref() {
        stream.set_read_timeout(Some(Duration::from_secs(60))).unwrap();
    }
    (server, Client { socket, next: 0, events: Vec::new() }, ws_url)
}

#[test]
fn navigate_mutate_and_screenshot() {
    let (_server, mut client, ws_url) = start();
    let port: u16 = ws_url.trim_start_matches("ws://127.0.0.1:").split('/').next().unwrap().parse().unwrap();

    // The lightpanda contract agent-browser relies on.
    let version: Value = serde_json::from_str(&http_get(port, "/json/version")).unwrap();
    assert_eq!(version["webSocketDebuggerUrl"], ws_url);

    let browser = client.call(None, "Browser.getVersion", json!({}));
    assert!(browser["product"].as_str().unwrap().starts_with("Kitesurf/"));

    let target = client.call(None, "Target.createTarget", json!({ "url": "about:blank" }))["targetId"].as_str().unwrap().to_string();
    let session = client.call(None, "Target.attachToTarget", json!({ "targetId": target, "flatten": true }))["sessionId"].as_str().unwrap().to_string();
    let s = Some(session.as_str());
    client.call(s, "Page.enable", json!({}));
    client.call(s, "Runtime.enable", json!({}));

    let url = format!("data:text/html;base64,{}", base64::engine::general_purpose::STANDARD.encode(PAGE));
    let navigated = client.call(s, "Page.navigate", json!({ "url": url }));
    assert!(navigated.get("errorText").is_none(), "navigate: {navigated}");
    client.wait_event("Page.loadEventFired");

    let console = client.wait_event("Runtime.consoleAPICalled");
    assert_eq!(console["params"]["args"][0]["value"], "script ran");

    let root = client.call(s, "DOM.getDocument", json!({ "depth": -1 }))["root"]["nodeId"].as_u64().unwrap();
    let div = client.call(s, "DOM.querySelector", json!({ "nodeId": root, "selector": "#root" }))["nodeId"].as_u64().unwrap();
    assert_ne!(div, 0);
    let html = client.call(s, "DOM.getOuterHTML", json!({ "nodeId": div }))["outerHTML"].as_str().unwrap().to_string();
    assert!(html.contains(r#"<p id="added">mutated by script</p>"#), "outerHTML: {html}");

    let text = client.call(s, "Runtime.evaluate", json!({ "expression": "document.getElementById('added').textContent", "returnByValue": true }));
    assert_eq!(text["result"]["value"], "mutated by script");

    let awaited = client.call(s, "Runtime.evaluate", json!({ "expression": "new Promise(r => setTimeout(() => r(6 * 7), 10))", "awaitPromise": true, "returnByValue": true }));
    assert_eq!(awaited["result"]["value"], 42);

    let href = client.call(s, "Runtime.evaluate", json!({ "expression": "location.href", "returnByValue": true }));
    assert!(href["result"]["value"].as_str().unwrap().starts_with("data:text/html"), "{href}");

    // A real click (hit testing, pointer events) runs the inline onclick attribute.
    for kind in ["mousePressed", "mouseReleased"] {
        client.call(s, "Input.dispatchMouseEvent", json!({ "type": kind, "x": 340, "y": 35, "button": "left", "clickCount": 1 }));
    }
    let pressed = client.call(s, "Runtime.evaluate", json!({ "expression": "document.getElementById('btn').textContent", "returnByValue": true }));
    assert_eq!(pressed["result"]["value"], "pressed");

    // Typing into a focused text field.
    for kind in ["mousePressed", "mouseReleased"] {
        client.call(s, "Input.dispatchMouseEvent", json!({ "type": kind, "x": 320, "y": 90, "button": "left", "clickCount": 1 }));
    }
    client.call(s, "Input.insertText", json!({ "text": "Ada" }));
    for (kind, key, text) in [("keyDown", "!", "!"), ("keyUp", "!", ""), ("rawKeyDown", "Backspace", ""), ("keyUp", "Backspace", ""), ("keyDown", "s", "s")] {
        client.call(s, "Input.dispatchKeyEvent", json!({ "type": kind, "key": key, "text": text }));
    }
    let typed = client.call(s, "Runtime.evaluate", json!({ "expression": "document.getElementById('name').value + ' ' + window.inputs", "returnByValue": true }));
    assert_eq!(typed["result"]["value"], "Adas 4");

    let thrown = client.call(s, "Runtime.evaluate", json!({ "expression": "null.boom" }));
    assert!(thrown.get("exceptionDetails").is_some(), "{thrown}");

    let element = client.call(s, "Runtime.evaluate", json!({ "expression": "document.querySelector('#added')" }))["result"].clone();
    assert_eq!(element["subtype"], "node");
    let called = client.call(
        s,
        "Runtime.callFunctionOn",
        json!({ "objectId": element["objectId"], "functionDeclaration": "function (suffix) { return this.textContent + suffix }", "arguments": [{ "value": "!" }], "returnByValue": true }),
    );
    assert_eq!(called["result"]["value"], "mutated by script!");

    let shot = client.call(s, "Page.captureScreenshot", json!({ "format": "png" }));
    let png_bytes = base64::engine::general_purpose::STANDARD.decode(shot["data"].as_str().unwrap()).unwrap();
    assert_eq!(&png_bytes[..8], b"\x89PNG\r\n\x1a\n");
    let decoder = png::Decoder::new(std::io::Cursor::new(&png_bytes));
    let mut reader = decoder.read_info().unwrap();
    let mut pixels = vec![0; reader.output_buffer_size().unwrap()];
    let info = reader.next_frame(&mut pixels).unwrap();
    assert_eq!((info.width, info.height), (1280, 720));
    let at = |x: usize, y: usize| {
        let i = (y * info.width as usize + x) * 4;
        [pixels[i], pixels[i + 1], pixels[i + 2]]
    };
    // Inside #root (painted red), outside it (white canvas).
    assert_eq!(at(190, 90), [255, 0, 0], "#root should be painted red");
    assert_eq!(at(600, 400), [255, 255, 255], "the canvas should be white");
    std::fs::write(std::env::temp_dir().join("kitesurf-cdp-smoke.png"), &png_bytes).unwrap();

    let _ = client.call(None, "Browser.close", json!({}));
}
