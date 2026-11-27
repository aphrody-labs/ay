//! The browser loop: owns every page, answers the protocol and emits its events.

use std::collections::HashMap;
use std::sync::mpsc::Receiver;
use std::sync::Arc;
use std::time::{Duration, Instant};

use base64::Engine;
use serde_json::{Value, json};
use tungstenite::Message;

use crate::net::{NetEvent, USER_AGENT};
use crate::page::{Clip, Env, Format, Page};
use crate::server::{Endpoint, Incoming, Socket, TargetSummary};

type Answer = Result<Value, (i64, String)>;

const LOAD_BUDGET: Duration = Duration::from_secs(5);
const DEFAULT_EVAL_TIMEOUT: Duration = Duration::from_secs(30);

fn invalid(message: impl Into<String>) -> (i64, String) {
    (-32602, message.into())
}

fn server_error(message: impl Into<String>) -> (i64, String) {
    (-32000, message.into())
}

struct Conn {
    id: u64,
    socket: Socket,
    alive: bool,
    discover: bool,
    auto_attach: bool,
    /// A `/devtools/page/<id>` socket talks to that page without session ids.
    implicit_session: Option<String>,
}

struct Session {
    target_id: String,
    conn: u64,
    implicit: bool,
    page: bool,
    runtime: bool,
    network: bool,
    lifecycle: bool,
}

enum Pending {
    Load(String),
    Navigate(String, String),
    Exit,
}

pub struct Browser {
    env: Env,
    endpoint: Arc<Endpoint>,
    incoming: Receiver<Incoming>,
    net_events: Receiver<NetEvent>,
    conns: Vec<Conn>,
    pages: Vec<Page>,
    sessions: HashMap<String, Session>,
    bindings: HashMap<String, Vec<String>>,
    pending: Vec<Pending>,
    next_id: u64,
    width: u32,
    height: u32,
    start: Instant,
}

impl Browser {
    pub fn new(env: Env, endpoint: Arc<Endpoint>, incoming: Receiver<Incoming>, net_events: Receiver<NetEvent>, width: u32, height: u32) -> Self {
        let mut browser = Self {
            env,
            endpoint,
            incoming,
            net_events,
            conns: Vec::new(),
            pages: Vec::new(),
            sessions: HashMap::new(),
            bindings: HashMap::new(),
            pending: Vec::new(),
            next_id: 1,
            width,
            height,
            start: Instant::now(),
        };
        browser.create_page();
        browser
    }

    fn next(&mut self) -> u64 {
        self.next_id += 1;
        self.next_id
    }

    fn timestamp(&self) -> f64 {
        self.start.elapsed().as_secs_f64()
    }

    pub fn run(mut self) -> ! {
        loop {
            while let Ok(incoming) = self.incoming.try_recv() {
                self.accept(incoming);
            }
            let mut busy = false;
            for i in 0..self.conns.len() {
                let mut texts = Vec::new();
                let conn = &mut self.conns[i];
                loop {
                    match conn.socket.read() {
                        Ok(Message::Text(text)) => texts.push(text.to_string()),
                        Ok(Message::Close(_)) => {
                            conn.alive = false;
                            break;
                        }
                        Ok(_) => {}
                        Err(tungstenite::Error::Io(e))
                            if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) =>
                        {
                            break;
                        }
                        Err(_) => {
                            conn.alive = false;
                            break;
                        }
                    }
                }
                let id = conn.id;
                busy |= !texts.is_empty();
                for text in texts {
                    self.handle(id, &text);
                    self.settle();
                }
            }
            self.reap();
            busy |= self.pump();
            self.settle();
            self.publish_targets();
            if !busy {
                std::thread::sleep(Duration::from_millis(2));
            }
        }
    }

    fn accept(&mut self, incoming: Incoming) {
        let id = self.next();
        let mut conn = Conn { id, socket: incoming.socket, alive: true, discover: false, auto_attach: false, implicit_session: None };
        if let Some(target) = incoming.path.strip_prefix("/devtools/page/") {
            if self.pages.iter().any(|p| p.target_id == target) {
                let session_id = format!("{:032X}", self.next());
                self.sessions.insert(session_id.clone(), Session::new(target.to_string(), id, true));
                conn.implicit_session = Some(session_id);
            }
        }
        self.conns.push(conn);
    }

    fn reap(&mut self) {
        let dead: Vec<u64> = self.conns.iter().filter(|c| !c.alive).map(|c| c.id).collect();
        if dead.is_empty() {
            return;
        }
        self.conns.retain(|c| c.alive);
        self.sessions.retain(|_, s| !dead.contains(&s.conn));
    }

    fn send(&mut self, conn: u64, value: Value) {
        if let Some(c) = self.conns.iter_mut().find(|c| c.id == conn && c.alive) {
            if c.socket.send(Message::text(value.to_string())).is_err() {
                c.alive = false;
            }
        }
    }

    /// An event to every session of `target` whose domain is enabled.
    fn page_event(&mut self, target: &str, method: &str, params: Value) {
        let domain = method.split('.').next().unwrap_or_default();
        let to: Vec<(u64, Option<String>)> = self
            .sessions
            .iter()
            .filter(|(_, s)| s.target_id == target && s.wants(domain, method))
            .map(|(id, s)| (s.conn, (!s.implicit).then(|| id.clone())))
            .collect();
        for (conn, session) in to {
            let mut event = json!({ "method": method, "params": params });
            if let Some(session) = session {
                event["sessionId"] = session.into();
            }
            self.send(conn, event);
        }
    }

    /// A `Target.*` event to every browser connection that discovers targets.
    fn target_event(&mut self, method: &str, params: Value) {
        let to: Vec<u64> = self.conns.iter().filter(|c| c.discover).map(|c| c.id).collect();
        for conn in to {
            self.send(conn, json!({ "method": method, "params": params }));
        }
    }

    fn page_index(&self, target: &str) -> Option<usize> {
        self.pages.iter().position(|p| p.target_id == target)
    }

    fn target_info(&mut self, i: usize) -> Value {
        let attached = self.sessions.values().any(|s| s.target_id == self.pages[i].target_id);
        let page = &mut self.pages[i];
        json!({
            "targetId": page.target_id,
            "type": "page",
            "title": title_or_url(page),
            "url": page.url,
            "attached": attached,
            "canAccessOpener": false,
            "browserContextId": "default",
        })
    }

    fn frame(&self, i: usize) -> Value {
        let page = &self.pages[i];
        let url = url::Url::parse(&page.url).ok();
        let origin = url.as_ref().map(|u| u.origin().ascii_serialization()).unwrap_or_else(|| "null".into());
        let secure = url.as_ref().is_some_and(|u| matches!(u.scheme(), "https" | "file" | "about" | "data") || u.host_str() == Some("localhost"));
        let mut frame = json!({
            "id": page.target_id,
            "loaderId": page.loader_id,
            "url": page.url.split('#').next().unwrap_or_default(),
            "domainAndRegistry": "",
            "securityOrigin": origin,
            "mimeType": "text/html",
            "adFrameStatus": { "adFrameType": "none" },
            "secureContextType": if secure { "Secure" } else { "InsecureScheme" },
            "crossOriginIsolatedContextType": "NotIsolated",
            "gatedAPIFeatures": [],
        });
        if let Some(fragment) = url.as_ref().and_then(|u| u.fragment()) {
            frame["urlFragment"] = format!("#{fragment}").into();
        }
        frame
    }

    fn context(&self, i: usize) -> Value {
        let page = &self.pages[i];
        let origin = url::Url::parse(&page.url).map(|u| u.origin().ascii_serialization()).unwrap_or_else(|_| "null".into());
        json!({
            "id": page.context_id,
            "origin": origin,
            "name": "",
            "uniqueId": format!("ks-context-{}", page.context_id),
            "auxData": { "isDefault": true, "type": "default", "frameId": page.target_id },
        })
    }

    fn create_page(&mut self) -> String {
        let target_id = format!("{:032X}", self.next().wrapping_mul(0x9E37_79B9_7F4A_7C15));
        let context_id = self.next();
        let page = Page::new(&self.env, target_id.clone(), context_id, self.width, self.height);
        self.pages.push(page);
        target_id
    }

    fn publish_targets(&mut self) {
        let summaries: Vec<TargetSummary> = self
            .pages
            .iter()
            .map(|p| TargetSummary { id: p.target_id.clone(), title: p.url.clone(), url: p.url.clone() })
            .collect();
        *self.endpoint.targets.lock().unwrap() = summaries;
    }

    fn attach(&mut self, conn: u64, target: &str) -> String {
        let session_id = format!("{:032X}", self.next().wrapping_mul(0x2545_F491_4F6C_DD1D));
        self.sessions.insert(session_id.clone(), Session::new(target.to_string(), conn, false));
        let i = self.page_index(target).unwrap();
        let info = self.target_info(i);
        self.send(conn, json!({ "method": "Target.attachedToTarget", "params": { "sessionId": session_id, "targetInfo": info, "waitingForDebugger": false } }));
        session_id
    }

    fn handle(&mut self, conn: u64, text: &str) {
        let Ok(message) = serde_json::from_str::<Value>(text) else {
            self.send(conn, json!({ "id": 0, "error": { "code": -32700, "message": "Message must be valid JSON" } }));
            return;
        };
        let id = message.get("id").cloned().unwrap_or(Value::Null);
        let method = message.get("method").and_then(Value::as_str).unwrap_or_default().to_string();
        let params = message.get("params").cloned().unwrap_or_else(|| json!({}));
        let explicit = message.get("sessionId").and_then(Value::as_str).map(str::to_string);
        let session = explicit.clone().or_else(|| self.conns.iter().find(|c| c.id == conn).and_then(|c| c.implicit_session.clone()));
        let answer = match &session {
            Some(s) if !self.sessions.contains_key(s) => Err(server_error(format!("Session with given id not found: {s}"))),
            _ => self.dispatch(conn, session.as_deref(), &method, &params),
        };
        let mut response = match answer {
            Ok(result) => json!({ "id": id, "result": result }),
            Err((code, message)) => json!({ "id": id, "error": { "code": code, "message": message } }),
        };
        if let Some(session) = explicit {
            response["sessionId"] = session.into();
        }
        self.send(conn, response);
    }

    /// Work that follows a response: page loads, deferred navigations, exit.
    fn settle(&mut self) {
        while !self.pending.is_empty() {
            match self.pending.remove(0) {
                Pending::Load(target) => self.finish_load(&target),
                Pending::Navigate(target, url) => {
                    let _ = self.navigate(&target, &url);
                }
                Pending::Exit => std::process::exit(0),
            }
        }
    }

    fn dispatch(&mut self, conn: u64, session: Option<&str>, method: &str, params: &Value) -> Answer {
        let (domain, command) = method.split_once('.').unwrap_or((method, ""));
        match domain {
            "Target" => self.target(conn, session, command, params),
            "Browser" => self.browser(command, params),
            _ => {
                let Some(session) = session else {
                    return match command {
                        "enable" | "disable" => Ok(json!({})),
                        _ => Err(server_error(format!("'{method}' wasn't found"))),
                    };
                };
                let target = self.sessions[session].target_id.clone();
                let Some(i) = self.page_index(&target) else {
                    return Err(server_error("No target with given id found"));
                };
                self.page_method(session, i, domain, command, params)
            }
        }
    }

    fn target(&mut self, conn: u64, session: Option<&str>, command: &str, params: &Value) -> Answer {
        match command {
            "getTargets" => {
                let infos: Vec<Value> = (0..self.pages.len()).map(|i| self.target_info(i)).collect();
                Ok(json!({ "targetInfos": infos }))
            }
            "getTargetInfo" => {
                let target = params["targetId"].as_str().map(str::to_string).or_else(|| session.map(|s| self.sessions[s].target_id.clone()));
                match target.and_then(|t| self.page_index(&t)) {
                    Some(i) => Ok(json!({ "targetInfo": self.target_info(i) })),
                    None => Ok(json!({ "targetInfo": { "targetId": self.endpoint.browser_id, "type": "browser", "title": "", "url": "", "attached": true, "canAccessOpener": false } })),
                }
            }
            "setDiscoverTargets" => {
                let discover = params["discover"].as_bool().unwrap_or(false);
                if let Some(c) = self.conns.iter_mut().find(|c| c.id == conn) {
                    c.discover = discover;
                }
                if discover {
                    for i in 0..self.pages.len() {
                        let info = self.target_info(i);
                        self.send(conn, json!({ "method": "Target.targetCreated", "params": { "targetInfo": info } }));
                    }
                }
                Ok(json!({}))
            }
            "setAutoAttach" => {
                if session.is_some() {
                    return Ok(json!({}));
                }
                let auto = params["autoAttach"].as_bool().unwrap_or(false);
                if let Some(c) = self.conns.iter_mut().find(|c| c.id == conn) {
                    c.auto_attach = auto;
                }
                if auto {
                    let targets: Vec<String> = self.pages.iter().map(|p| p.target_id.clone()).collect();
                    for target in targets {
                        if !self.sessions.values().any(|s| s.conn == conn && s.target_id == target) {
                            self.attach(conn, &target);
                        }
                    }
                }
                Ok(json!({}))
            }
            "createTarget" => {
                let target = self.create_page();
                let i = self.pages.len() - 1;
                let info = self.target_info(i);
                self.target_event("Target.targetCreated", json!({ "targetInfo": info }));
                let auto: Vec<u64> = self.conns.iter().filter(|c| c.auto_attach).map(|c| c.id).collect();
                for c in auto {
                    self.attach(c, &target);
                }
                let url = params["url"].as_str().unwrap_or("about:blank");
                if url != "about:blank" && !url.is_empty() {
                    self.pending.push(Pending::Navigate(target.clone(), url.to_string()));
                }
                Ok(json!({ "targetId": target }))
            }
            "attachToTarget" => {
                let target = params["targetId"].as_str().unwrap_or_default().to_string();
                if self.page_index(&target).is_none() {
                    return Err(invalid("No target with given id found"));
                }
                Ok(json!({ "sessionId": self.attach(conn, &target) }))
            }
            "attachToBrowserTarget" => Err(server_error("attachToBrowserTarget is not supported")),
            "detachFromTarget" => {
                let session_id = params["sessionId"].as_str().map(str::to_string).or_else(|| {
                    let target = params["targetId"].as_str()?;
                    self.sessions.iter().find(|(_, s)| s.conn == conn && s.target_id == target).map(|(id, _)| id.clone())
                });
                let Some(session_id) = session_id else { return Err(invalid("No session with given id found")) };
                if let Some(s) = self.sessions.remove(&session_id) {
                    self.send(s.conn, json!({ "method": "Target.detachedFromTarget", "params": { "sessionId": session_id, "targetId": s.target_id } }));
                }
                Ok(json!({}))
            }
            "closeTarget" => {
                let target = params["targetId"].as_str().unwrap_or_default().to_string();
                self.close_target(&target).map(|_| json!({ "success": true }))
            }
            "activateTarget" | "setRemoteLocations" | "exposeDevToolsProtocol" => Ok(json!({})),
            "createBrowserContext" => Ok(json!({ "browserContextId": format!("ctx-{}", self.next()) })),
            "disposeBrowserContext" => Ok(json!({})),
            "getBrowserContexts" => Ok(json!({ "browserContextIds": [] })),
            _ => Err(server_error(format!("'Target.{command}' wasn't found"))),
        }
    }

    fn close_target(&mut self, target: &str) -> Result<(), (i64, String)> {
        let Some(i) = self.page_index(target) else { return Err(invalid("No target with given id found")) };
        self.pages.remove(i);
        let sessions: Vec<(String, u64)> = self.sessions.iter().filter(|(_, s)| s.target_id == target).map(|(id, s)| (id.clone(), s.conn)).collect();
        for (session_id, conn) in sessions {
            self.sessions.remove(&session_id);
            self.send(conn, json!({ "method": "Target.detachedFromTarget", "params": { "sessionId": session_id, "targetId": target } }));
        }
        self.target_event("Target.targetDestroyed", json!({ "targetId": target }));
        Ok(())
    }

    fn browser(&mut self, command: &str, params: &Value) -> Answer {
        match command {
            "getVersion" => Ok(json!({
                "protocolVersion": "1.3",
                "product": concat!("Kitesurf/", env!("CARGO_PKG_VERSION")),
                "revision": "0",
                "userAgent": USER_AGENT,
                "jsVersion": "Boa 0.22",
            })),
            "close" => {
                self.pending.push(Pending::Exit);
                Ok(json!({}))
            }
            "getWindowForTarget" => Ok(json!({ "windowId": 1, "bounds": { "left": 0, "top": 0, "width": self.width, "height": self.height, "windowState": "normal" } })),
            "setWindowBounds" | "setContentsSize" => {
                let bounds = if command == "setWindowBounds" { &params["bounds"] } else { params };
                if let (Some(w), Some(h)) = (bounds["width"].as_u64(), bounds["height"].as_u64()) {
                    self.width = w as u32;
                    self.height = h as u32;
                    for page in &mut self.pages {
                        let scale = page.scale;
                        page.set_viewport(w as u32, h as u32, scale);
                    }
                }
                Ok(json!({}))
            }
            "setDownloadBehavior" | "grantPermissions" | "resetPermissions" | "setPermission" | "crashGpuProcess" => Ok(json!({})),
            _ => Err(server_error(format!("'Browser.{command}' wasn't found"))),
        }
    }

    fn page_method(&mut self, session: &str, i: usize, domain: &str, command: &str, params: &Value) -> Answer {
        let target = self.pages[i].target_id.clone();
        match (domain, command) {
            ("Page", "enable") => {
                self.sessions.get_mut(session).unwrap().page = true;
                Ok(json!({}))
            }
            ("Page", "disable") => {
                self.sessions.get_mut(session).unwrap().page = false;
                Ok(json!({}))
            }
            ("Page", "setLifecycleEventsEnabled") => {
                self.sessions.get_mut(session).unwrap().lifecycle = params["enabled"].as_bool().unwrap_or(false);
                Ok(json!({}))
            }
            ("Page", "getFrameTree") => Ok(json!({ "frameTree": { "frame": self.frame(i), "childFrames": [] } })),
            ("Page", "getResourceTree") => Ok(json!({ "frameTree": { "frame": self.frame(i), "childFrames": [], "resources": [] } })),
            ("Page", "navigate") => {
                let url = params["url"].as_str().ok_or_else(|| invalid("url: string value expected"))?.to_string();
                match self.navigate(&target, &url) {
                    Ok(loader) => Ok(json!({ "frameId": target, "loaderId": loader })),
                    Err(error) => Ok(json!({ "frameId": target, "errorText": error })),
                }
            }
            ("Page", "reload") => {
                let url = self.pages[i].url.clone();
                self.navigate(&target, &url).map_err(server_error)?;
                Ok(json!({}))
            }
            ("Page", "getNavigationHistory") => {
                let page = &self.pages[i];
                let entries: Vec<Value> = page
                    .history
                    .iter()
                    .enumerate()
                    .map(|(n, url)| json!({ "id": n, "url": url, "userTypedURL": url, "title": url, "transitionType": "typed" }))
                    .collect();
                Ok(json!({ "currentIndex": page.history_index, "entries": entries }))
            }
            ("Page", "navigateToHistoryEntry") => {
                let n = params["entryId"].as_u64().unwrap_or(0) as usize;
                let Some(url) = self.pages[i].history.get(n).cloned() else { return Err(invalid("No entry with passed id")) };
                self.pages[i].history_index = n;
                self.navigate(&target, &url).map_err(server_error)?;
                Ok(json!({}))
            }
            ("Page", "resetNavigationHistory") => {
                let page = &mut self.pages[i];
                page.history = vec![page.url.clone()];
                page.history_index = 0;
                Ok(json!({}))
            }
            ("Page", "addScriptToEvaluateOnNewDocument") => {
                let id = self.next().to_string();
                let source = params["source"].as_str().unwrap_or_default().to_string();
                self.pages[i].init_scripts.push((id.clone(), source));
                Ok(json!({ "identifier": id }))
            }
            ("Page", "removeScriptToEvaluateOnNewDocument") => {
                let id = params["identifier"].as_str().unwrap_or_default();
                self.pages[i].init_scripts.retain(|(s, _)| s != id);
                Ok(json!({}))
            }
            ("Page", "createIsolatedWorld") => Ok(json!({ "executionContextId": self.pages[i].context_id })),
            ("Page", "captureScreenshot") => {
                let format = match params["format"].as_str().unwrap_or("png") {
                    "jpeg" => Format::Jpeg(params["quality"].as_u64().unwrap_or(80) as u8),
                    _ => Format::Png,
                };
                let clip = params.get("clip").filter(|c| c.is_object()).map(|c| Clip {
                    x: c["x"].as_f64().unwrap_or(0.0),
                    y: c["y"].as_f64().unwrap_or(0.0),
                    width: c["width"].as_f64().unwrap_or(self.width as f64),
                    height: c["height"].as_f64().unwrap_or(self.height as f64),
                    scale: c["scale"].as_f64().unwrap_or(1.0),
                });
                let bytes = self.pages[i].screenshot(format, clip, false).map_err(server_error)?;
                Ok(json!({ "data": base64::engine::general_purpose::STANDARD.encode(bytes) }))
            }
            ("Page", "getLayoutMetrics") => {
                let page = &mut self.pages[i];
                let (cw, ch) = page.content_size();
                let (sx, sy) = page.scroll();
                let (w, h) = (page.width, page.height);
                let layout = json!({ "pageX": sx, "pageY": sy, "clientWidth": w, "clientHeight": h });
                let visual = json!({ "offsetX": 0, "offsetY": 0, "pageX": sx, "pageY": sy, "clientWidth": w, "clientHeight": h, "scale": 1, "zoom": 1 });
                let content = json!({ "x": 0, "y": 0, "width": cw, "height": ch });
                Ok(json!({
                    "layoutViewport": layout, "visualViewport": visual, "contentSize": content,
                    "cssLayoutViewport": layout, "cssVisualViewport": visual, "cssContentSize": content,
                }))
            }
            ("Page", "setDocumentContent") => {
                let html = params["html"].as_str().unwrap_or_default().to_string();
                let url = url::Url::parse(&self.pages[i].url).unwrap_or_else(|_| url::Url::parse("about:blank").unwrap());
                let old = self.pages[i].context_id;
                let context = self.next();
                self.pages[i].set_content(&self.env, &url, &html, context);
                self.rebind(i);
                self.commit_events(&target, old);
                self.pending.push(Pending::Load(target));
                Ok(json!({}))
            }
            ("Page", "close") => {
                self.close_target(&target)?;
                Ok(json!({}))
            }
            ("Page", "printToPDF") => Err(server_error("Printing is not available")),
            ("Page", "startScreencast" | "stopScreencast" | "screencastFrameAck" | "bringToFront" | "setBypassCSP" | "setWebLifecycleState"
            | "setInterceptFileChooserDialog" | "stopLoading" | "handleJavaScriptDialog" | "setFontFamilies" | "setFontSizes" | "addCompilationCache"
            | "setAdBlockingEnabled" | "setDownloadBehavior" | "setRPHRegistrationMode" | "setSPCTransactionMode") => Ok(json!({})),

            ("Runtime", "enable") => {
                self.sessions.get_mut(session).unwrap().runtime = true;
                let context = self.context(i);
                self.page_event(&target, "Runtime.executionContextCreated", json!({ "context": context }));
                Ok(json!({}))
            }
            ("Runtime", "disable") => {
                self.sessions.get_mut(session).unwrap().runtime = false;
                Ok(json!({}))
            }
            ("Runtime", "evaluate") => {
                let expression = params["expression"].as_str().unwrap_or_default();
                let options = run_options(params);
                let timeout = params["timeout"].as_u64().map(Duration::from_millis).unwrap_or(DEFAULT_EVAL_TIMEOUT);
                let answer = self.pages[i].evaluate(expression, &options, timeout).map_err(server_error)?;
                self.finish_run(i, answer)
            }
            ("Runtime", "callFunctionOn") => {
                let declaration = params["functionDeclaration"].as_str().unwrap_or_default();
                let options = run_options(params);
                let args = params.get("arguments").cloned().unwrap_or_else(|| json!([]));
                let answer = self.pages[i].call_function(params["objectId"].as_str(), declaration, &args, &options, DEFAULT_EVAL_TIMEOUT).map_err(server_error)?;
                self.finish_run(i, answer)
            }
            ("Runtime", "getProperties") => {
                let object = params["objectId"].as_str().unwrap_or_default();
                let own = params["ownProperties"].as_bool().unwrap_or(false);
                let answer = self.pages[i].ks(&format!("properties({}, {own})", js(object))).map_err(server_error)?.unwrap_or_default();
                checked(answer)
            }
            ("Runtime", "releaseObject") => {
                let object = params["objectId"].as_str().unwrap_or_default();
                self.pages[i].ks(&format!("release({})", js(object))).map_err(server_error)?;
                Ok(json!({}))
            }
            ("Runtime", "releaseObjectGroup") => {
                let group = params["objectGroup"].as_str().unwrap_or_default();
                self.pages[i].ks(&format!("releaseGroup({})", js(group))).map_err(server_error)?;
                Ok(json!({}))
            }
            ("Runtime", "addBinding") => {
                let name = params["name"].as_str().unwrap_or_default().to_string();
                self.pages[i].ks(&format!("addBinding({})", js(&name))).map_err(server_error)?;
                let names = self.bindings.entry(target).or_default();
                if !names.contains(&name) {
                    names.push(name);
                }
                Ok(json!({}))
            }
            ("Runtime", "removeBinding") => {
                let name = params["name"].as_str().unwrap_or_default();
                if let Some(names) = self.bindings.get_mut(&target) {
                    names.retain(|n| n != name);
                }
                Ok(json!({}))
            }
            ("Runtime", "runIfWaitingForDebugger" | "discardConsoleEntries" | "setAsyncCallStackDepth" | "setCustomObjectFormatterEnabled" | "setMaxCallStackSizeToCapture") => Ok(json!({})),
            ("Runtime", "getIsolateId") => Ok(json!({ "id": "kitesurf" })),

            ("DOM", "enable" | "disable") => Ok(json!({})),
            ("DOM", "getDocument") => {
                let depth = params["depth"].as_i64().unwrap_or(1);
                let root = self.pages[i].ks(&format!("document({depth})")).map_err(server_error)?.unwrap_or_default();
                Ok(json!({ "root": root }))
            }
            ("DOM", "describeNode") => {
                let node = self.node_param(i, params)?;
                let depth = params["depth"].as_i64().unwrap_or(0);
                let node = self.pages[i].ks(&format!("describe({node}, {depth})")).map_err(server_error)?.ok_or_else(|| invalid("Could not find node with given id"))?;
                Ok(json!({ "node": node }))
            }
            ("DOM", "querySelector") => {
                let node = self.node_param(i, params)?;
                let selector = params["selector"].as_str().unwrap_or_default();
                let found = self.pages[i].ks_text(&format!("querySelector({node}, {})", js(selector))).map_err(server_error)?;
                Ok(json!({ "nodeId": found.parse::<u64>().unwrap_or(0) }))
            }
            ("DOM", "querySelectorAll") => {
                let node = self.node_param(i, params)?;
                let selector = params["selector"].as_str().unwrap_or_default();
                let found = self.pages[i].ks(&format!("querySelectorAll({node}, {})", js(selector))).map_err(server_error)?.unwrap_or_else(|| json!([]));
                Ok(json!({ "nodeIds": found }))
            }
            ("DOM", "getOuterHTML") => {
                let node = self.node_param(i, params)?;
                let html = self.pages[i].ks_text(&format!("outerHTML({node})")).map_err(server_error)?;
                Ok(json!({ "outerHTML": html }))
            }
            ("DOM", "setOuterHTML") => {
                let node = self.node_param(i, params)?;
                let html = params["outerHTML"].as_str().unwrap_or_default();
                self.pages[i].ks(&format!("setOuterHTML({node}, {})", js(html))).map_err(server_error)?;
                Ok(json!({}))
            }
            ("DOM", "resolveNode") => {
                let node = self.node_param(i, params)?;
                let group = params["objectGroup"].as_str().unwrap_or_default();
                let object = self.pages[i].ks(&format!("nodeObject({node}, {})", js(group))).map_err(server_error)?.ok_or_else(|| invalid("No node with given id found"))?;
                Ok(json!({ "object": object }))
            }
            ("DOM", "requestNode") => Ok(json!({ "nodeId": self.node_param(i, params)? })),
            ("DOM", "pushNodesByBackendIdsToFrontend") => Ok(json!({ "nodeIds": params["backendNodeIds"].clone() })),
            ("DOM", "getBoxModel") => {
                let node = self.node_param(i, params)?;
                self.pages[i].resolve();
                self.pages[i].ks(&format!("boxModel({node})")).map_err(server_error)?.ok_or_else(|| server_error("Could not compute box model."))
            }
            ("DOM", "getContentQuads") => {
                let node = self.node_param(i, params)?;
                self.pages[i].resolve();
                let model = self.pages[i].ks(&format!("boxModel({node})")).map_err(server_error)?.ok_or_else(|| server_error("Could not compute content quads."))?;
                Ok(json!({ "quads": [model["model"]["content"].clone()] }))
            }
            ("DOM", "scrollIntoViewIfNeeded") => {
                let node = self.node_param(i, params)?;
                self.pages[i].ks(&format!("scrollIntoView({node})")).map_err(server_error)?;
                Ok(json!({}))
            }
            ("DOM", "focus") => {
                let node = self.node_param(i, params)?;
                self.pages[i].ks(&format!("focus({node})")).map_err(server_error)?;
                Ok(json!({}))
            }
            ("DOM", "setAttributeValue") => {
                let node = self.node_param(i, params)?;
                let (name, value) = (params["name"].as_str().unwrap_or_default(), params["value"].as_str().unwrap_or_default());
                self.pages[i].ks(&format!("setAttribute({node}, {}, {})", js(name), js(value))).map_err(server_error)?;
                Ok(json!({}))
            }
            ("DOM", "removeAttribute") => {
                let node = self.node_param(i, params)?;
                let name = params["name"].as_str().unwrap_or_default();
                self.pages[i].ks(&format!("removeAttribute({node}, {})", js(name))).map_err(server_error)?;
                Ok(json!({}))
            }
            ("DOM", "getNodeForLocation") => {
                let (x, y) = (params["x"].as_f64().unwrap_or(0.0), params["y"].as_f64().unwrap_or(0.0));
                self.pages[i].resolve();
                let node = self.pages[i].ks_text(&format!("nodeAt({x}, {y})")).map_err(server_error)?.parse::<u64>().unwrap_or(0);
                if node == 0 {
                    return Err(server_error("No node found at given location"));
                }
                Ok(json!({ "backendNodeId": node, "nodeId": node, "frameId": target }))
            }
            ("DOM", "getFrameOwner") => Err(server_error("Frame with the given id does not belong to the target")),
            ("DOM", "setFileInputFiles") => Err(server_error("File inputs are not supported")),

            ("Input", "dispatchMouseEvent") => {
                let page = &mut self.pages[i];
                page.mouse(
                    params["type"].as_str().unwrap_or("mouseMoved"),
                    params["x"].as_f64().unwrap_or(0.0) as f32,
                    params["y"].as_f64().unwrap_or(0.0) as f32,
                    params["button"].as_str().unwrap_or("none"),
                    params["buttons"].as_u64().unwrap_or(0) as u8,
                    (params["deltaX"].as_f64().unwrap_or(0.0), params["deltaY"].as_f64().unwrap_or(0.0)),
                    params["modifiers"].as_u64().unwrap_or(0),
                );
                Ok(json!({}))
            }
            ("Input", "dispatchTouchEvent") => {
                let kind = match params["type"].as_str().unwrap_or_default() {
                    "touchStart" => "mousePressed",
                    "touchEnd" | "touchCancel" => "mouseReleased",
                    _ => "mouseMoved",
                };
                let point = &params["touchPoints"][0];
                let (x, y) = (point["x"].as_f64().unwrap_or(0.0) as f32, point["y"].as_f64().unwrap_or(0.0) as f32);
                let buttons = if kind == "mouseReleased" { 0 } else { 1 };
                self.pages[i].mouse(kind, x, y, "left", buttons, (0.0, 0.0), params["modifiers"].as_u64().unwrap_or(0));
                Ok(json!({}))
            }
            ("Input", "dispatchKeyEvent") => {
                self.pages[i].key(
                    params["type"].as_str().unwrap_or("keyDown"),
                    params["key"].as_str().unwrap_or_default(),
                    params["code"].as_str().unwrap_or_default(),
                    params["text"].as_str(),
                    params["modifiers"].as_u64().unwrap_or(0),
                    params["autoRepeat"].as_bool().unwrap_or(false),
                );
                Ok(json!({}))
            }
            ("Input", "insertText") => {
                let text = params["text"].as_str().unwrap_or_default();
                self.pages[i].insert_text(text).map_err(server_error)?;
                Ok(json!({}))
            }
            ("Input", "setIgnoreInputEvents" | "setInterceptDrags") => Ok(json!({})),

            ("Emulation", "setDeviceMetricsOverride") => {
                let page = &mut self.pages[i];
                let width = params["width"].as_u64().filter(|w| *w > 0).map(|w| w as u32).unwrap_or(page.width);
                let height = params["height"].as_u64().filter(|h| *h > 0).map(|h| h as u32).unwrap_or(page.height);
                let scale = params["deviceScaleFactor"].as_f64().filter(|s| *s > 0.0).unwrap_or(1.0) as f32;
                page.set_viewport(width, height, scale);
                Ok(json!({}))
            }
            ("Emulation", "clearDeviceMetricsOverride") => {
                let (w, h) = (self.width, self.height);
                self.pages[i].set_viewport(w, h, 1.0);
                Ok(json!({}))
            }
            ("Emulation", "setUserAgentOverride") => {
                if let Some(ua) = params["userAgent"].as_str().filter(|ua| !ua.is_empty()) {
                    *self.env.net.user_agent.lock().unwrap() = ua.to_string();
                }
                Ok(json!({}))
            }
            ("Emulation", _) => Ok(json!({})),

            ("Network", "enable") => {
                self.sessions.get_mut(session).unwrap().network = true;
                Ok(json!({}))
            }
            ("Network", "disable") => {
                self.sessions.get_mut(session).unwrap().network = false;
                Ok(json!({}))
            }
            ("Network", "setExtraHTTPHeaders") => {
                let headers = params["headers"].as_object().map(|h| h.iter().map(|(k, v)| (k.clone(), v.as_str().unwrap_or_default().to_string())).collect()).unwrap_or_default();
                *self.env.net.extra_headers.lock().unwrap() = headers;
                Ok(json!({}))
            }
            ("Network", "setUserAgentOverride") => {
                if let Some(ua) = params["userAgent"].as_str().filter(|ua| !ua.is_empty()) {
                    *self.env.net.user_agent.lock().unwrap() = ua.to_string();
                }
                Ok(json!({}))
            }
            ("Network", "getCookies" | "getAllCookies") => Ok(json!({ "cookies": [] })),
            ("Network", "setCookie") => Ok(json!({ "success": false })),
            ("Network", "getResponseBody") => Err(server_error("No resource with given identifier found")),
            ("Network", _) => Ok(json!({})),
            ("Storage", "getCookies") => Ok(json!({ "cookies": [] })),

            ("Accessibility", "getFullAXTree" | "getPartialAXTree" | "queryAXTree") => {
                self.pages[i].ks("axTree()").map_err(server_error)?.ok_or_else(|| server_error("No document"))
            }
            ("Performance", "getMetrics") => Ok(json!({ "metrics": [] })),
            ("Fetch", "enable") => Err(server_error("Request interception is not supported")),
            ("Accessibility" | "Log" | "Security" | "Performance" | "CSS" | "Overlay" | "Inspector" | "ServiceWorker" | "Fetch" | "Storage" | "Audits"
            | "Debugger" | "Profiler" | "HeapProfiler" | "Animation" | "LayerTree" | "Media" | "WebAuthn" | "DOMStorage" | "IndexedDB" | "CacheStorage"
            | "BackgroundService" | "Autofill", _) => Ok(json!({})),
            (_, "enable" | "disable") => Ok(json!({})),
            _ => Err(server_error(format!("'{domain}.{command}' wasn't found"))),
        }
    }

    /// Resolve `nodeId`, `backendNodeId` or `objectId` to a node id (0 when absent).
    fn node_param(&mut self, i: usize, params: &Value) -> Result<u64, (i64, String)> {
        if let Some(id) = params["nodeId"].as_u64().or_else(|| params["backendNodeId"].as_u64()) {
            return Ok(id);
        }
        if let Some(object) = params["objectId"].as_str() {
            let id = self.pages[i].ks_text(&format!("nodeIdOf({})", js(object))).map_err(server_error)?;
            return match id.parse::<u64>() {
                Ok(id) if id > 0 => Ok(id),
                _ => Err(invalid("Node is not found for the given object")),
            };
        }
        Err(invalid("Either nodeId, backendNodeId or objectId must be specified"))
    }

    fn finish_run(&self, i: usize, mut answer: Value) -> Answer {
        if let Some(error) = answer.get("error").and_then(Value::as_str) {
            return Err(server_error(error.to_string()));
        }
        if let Some(details) = answer.get_mut("exceptionDetails") {
            details["executionContextId"] = self.pages[i].context_id.into();
        }
        Ok(answer)
    }

    fn rebind(&mut self, i: usize) {
        let names = self.bindings.get(&self.pages[i].target_id).cloned().unwrap_or_default();
        for name in names {
            let _ = self.pages[i].ks(&format!("addBinding({})", js(&name)));
        }
    }

    /// Fetch and commit a new document; its scripts run in [`Browser::finish_load`], after the response.
    fn navigate(&mut self, target: &str, url: &str) -> Result<String, String> {
        let Some(i) = self.page_index(target) else { return Err("No target with given id found".into()) };
        let ts = self.timestamp();
        self.page_event(target, "Page.frameStartedLoading", json!({ "frameId": target }));
        let request_id = self.env.net.request_id();
        let loader = format!("{:016X}", self.next_id + 1);
        self.page_event(
            target,
            "Network.requestWillBeSent",
            json!({
                "requestId": request_id, "loaderId": loader, "documentURL": url, "frameId": target, "type": "Document",
                "request": { "url": url, "method": "GET", "headers": {}, "initialPriority": "VeryHigh", "referrerPolicy": "strict-origin-when-cross-origin" },
                "timestamp": ts, "wallTime": wall_time(), "initiator": { "type": "other" }, "redirectHasExtraInfo": false,
            }),
        );
        let old = self.pages[i].context_id;
        let context = self.next();
        if let Err(error) = self.pages[i].open(&self.env, url, context) {
            let ts = self.timestamp();
            self.page_event(target, "Network.loadingFailed", json!({ "requestId": request_id, "timestamp": ts, "type": "Document", "errorText": error, "canceled": false }));
            self.page_event(target, "Page.frameStoppedLoading", json!({ "frameId": target }));
            return Err(error);
        }
        self.pages[i].push_history();
        self.rebind(i);
        let page_url = self.pages[i].url.clone();
        let ts = self.timestamp();
        self.page_event(
            target,
            "Network.responseReceived",
            json!({
                "requestId": request_id, "loaderId": self.pages[i].loader_id, "timestamp": ts, "type": "Document", "frameId": target,
                "response": { "url": page_url, "status": 200, "statusText": "OK", "headers": {}, "mimeType": "text/html", "connectionReused": false, "connectionId": 0, "encodedDataLength": 0, "securityState": "unknown" },
                "hasExtraInfo": false,
            }),
        );
        self.page_event(target, "Network.loadingFinished", json!({ "requestId": request_id, "timestamp": ts, "encodedDataLength": 0 }));
        self.commit_events(target, old);
        self.pending.push(Pending::Load(target.to_string()));
        Ok(self.pages[i].loader_id.clone())
    }

    fn commit_events(&mut self, target: &str, old_context: u64) {
        let Some(i) = self.page_index(target) else { return };
        self.page_event(target, "Runtime.executionContextDestroyed", json!({ "executionContextId": old_context, "executionContextUniqueId": format!("ks-context-{old_context}") }));
        self.page_event(target, "Runtime.executionContextsCleared", json!({}));
        let frame = self.frame(i);
        self.page_event(target, "Page.frameNavigated", json!({ "frame": frame, "type": "Navigation" }));
        let info = self.target_info(i);
        self.target_event("Target.targetInfoChanged", json!({ "targetInfo": info }));
        let context = self.context(i);
        self.page_event(target, "Runtime.executionContextCreated", json!({ "context": context }));
        self.lifecycle(i, "init");
    }

    fn lifecycle(&mut self, i: usize, name: &str) {
        let (target, loader) = (self.pages[i].target_id.clone(), self.pages[i].loader_id.clone());
        let ts = self.timestamp();
        self.page_event(&target, "Page.lifecycleEvent", json!({ "frameId": target, "loaderId": loader, "name": name, "timestamp": ts }));
    }

    fn finish_load(&mut self, target: &str) {
        let Some(i) = self.page_index(target) else { return };
        self.pages[i].load(LOAD_BUDGET);
        self.flush_console(i);
        let ts = self.timestamp();
        self.page_event(target, "Page.domContentEventFired", json!({ "timestamp": ts }));
        self.lifecycle(i, "DOMContentLoaded");
        self.page_event(target, "Page.loadEventFired", json!({ "timestamp": ts }));
        self.lifecycle(i, "load");
        self.page_event(target, "Page.frameStoppedLoading", json!({ "frameId": target }));
        self.lifecycle(i, "networkAlmostIdle");
        self.lifecycle(i, "networkIdle");
        let info = self.target_info(i);
        self.target_event("Target.targetInfoChanged", json!({ "targetInfo": info }));
    }

    fn flush_console(&mut self, i: usize) {
        let messages = self.pages[i].doc.take_console_messages();
        let errors = self.pages[i].doc.take_js_errors();
        let (target, context) = (self.pages[i].target_id.clone(), self.pages[i].context_id);
        for (level, text) in messages {
            let ts = wall_time() * 1000.0;
            self.page_event(
                &target,
                "Runtime.consoleAPICalled",
                json!({ "type": level, "args": [{ "type": "string", "value": text }], "executionContextId": context, "timestamp": ts, "stackTrace": { "callFrames": [] } }),
            );
        }
        for text in errors {
            let ts = wall_time() * 1000.0;
            self.page_event(
                &target,
                "Runtime.exceptionThrown",
                json!({ "timestamp": ts, "exceptionDetails": { "exceptionId": 1, "text": text, "lineNumber": 0, "columnNumber": 0, "executionContextId": context } }),
            );
        }
        if self.bindings.get(&target).is_some_and(|b| !b.is_empty()) {
            if let Ok(Some(Value::Array(calls))) = self.pages[i].ks("takeBindings()") {
                for call in calls {
                    let name = call[0].as_str().unwrap_or_default().to_string();
                    let payload = call[1].as_str().unwrap_or_default().to_string();
                    self.page_event(&target, "Runtime.bindingCalled", json!({ "name": name, "payload": payload, "executionContextId": context }));
                }
            }
        }
    }

    /// Timers, resources, console output, link navigations and network events of every page.
    fn pump(&mut self) -> bool {
        let mut busy = false;
        for i in 0..self.pages.len() {
            busy |= self.pages[i].pump();
            self.flush_console(i);
        }
        let navigations: Vec<(usize, url::Url)> = std::mem::take(&mut *self.env.navigations.lock().unwrap());
        for (doc_id, url) in navigations {
            if let Some(i) = self.pages.iter().position(|p| p.doc_id() == doc_id) {
                let target = self.pages[i].target_id.clone();
                self.pending.push(Pending::Navigate(target, url.to_string()));
                busy = true;
            }
        }
        while let Ok(event) = self.net_events.try_recv() {
            busy = true;
            let ts = self.timestamp();
            let doc_id = match &event {
                NetEvent::Started { doc_id, .. } | NetEvent::Finished { doc_id, .. } | NetEvent::Failed { doc_id, .. } => *doc_id,
            };
            let Some(i) = self.pages.iter().position(|p| p.doc_id() == doc_id) else { continue };
            let (target, loader) = (self.pages[i].target_id.clone(), self.pages[i].loader_id.clone());
            match event {
                NetEvent::Started { request_id, url, method, .. } => self.page_event(
                    &target,
                    "Network.requestWillBeSent",
                    json!({
                        "requestId": request_id, "loaderId": loader, "documentURL": self.pages[i].url, "frameId": target, "type": "Other",
                        "request": { "url": url, "method": method, "headers": {}, "initialPriority": "Low", "referrerPolicy": "strict-origin-when-cross-origin" },
                        "timestamp": ts, "wallTime": wall_time(), "initiator": { "type": "parser" }, "redirectHasExtraInfo": false,
                    }),
                ),
                NetEvent::Finished { request_id, status, mime_type, headers, length, .. } => {
                    let headers: serde_json::Map<String, Value> = headers.into_iter().map(|(k, v)| (k, Value::String(v))).collect();
                    self.page_event(
                        &target,
                        "Network.responseReceived",
                        json!({
                            "requestId": request_id, "loaderId": loader, "timestamp": ts, "type": "Other", "frameId": target, "hasExtraInfo": false,
                            "response": { "url": "", "status": status, "statusText": "", "headers": headers, "mimeType": mime_type, "connectionReused": false, "connectionId": 0, "encodedDataLength": length, "securityState": "unknown" },
                        }),
                    );
                    self.page_event(&target, "Network.loadingFinished", json!({ "requestId": request_id, "timestamp": ts, "encodedDataLength": length }));
                }
                NetEvent::Failed { request_id, error, .. } => self.page_event(
                    &target,
                    "Network.loadingFailed",
                    json!({ "requestId": request_id, "timestamp": ts, "type": "Other", "errorText": error, "canceled": false }),
                ),
            }
        }
        busy
    }
}

impl Session {
    fn new(target_id: String, conn: u64, implicit: bool) -> Self {
        Self { target_id, conn, implicit, page: false, runtime: false, network: false, lifecycle: false }
    }

    fn wants(&self, domain: &str, method: &str) -> bool {
        match domain {
            "Page" if method == "Page.lifecycleEvent" => self.page && self.lifecycle,
            "Page" => self.page,
            "Runtime" => self.runtime,
            "Network" => self.network,
            _ => true,
        }
    }
}

fn title_or_url(page: &mut Page) -> String {
    let title = page.title();
    if title.is_empty() { page.url.clone() } else { title }
}

fn run_options(params: &Value) -> Value {
    json!({
        "returnByValue": params["returnByValue"].as_bool().unwrap_or(false),
        "awaitPromise": params["awaitPromise"].as_bool().unwrap_or(false),
        "group": params["objectGroup"].as_str().unwrap_or_default(),
    })
}

fn checked(answer: Value) -> Answer {
    match answer.get("error").and_then(Value::as_str) {
        Some(error) => Err(server_error(error.to_string())),
        None => Ok(answer),
    }
}

fn js(s: &str) -> String {
    Value::from(s).to_string()
}

fn wall_time() -> f64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.0)
}
