//! Resource loading: `data:`, `file:` and `http(s):` (ureq over rustls with the RustCrypto provider).

use std::io::Read;
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use blitz_traits::net::{Body, NetHandler, NetProvider, Request};
use blitz_traits::navigation::{NavigationOptions, NavigationProvider};
use blitz_vibey_script::{FetchError, ScriptFetcher};
use url::Url;

pub const USER_AGENT: &str = concat!(
    "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Kitesurf/",
    env!("CARGO_PKG_VERSION"),
    " Safari/537.36"
);
const MAX_BODY: u64 = 64 << 20;

pub struct Response {
    pub url: String,
    pub status: u16,
    pub mime_type: String,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

/// A network event for the CDP `Network` domain, sent to the browser loop.
pub enum NetEvent {
    Started { request_id: String, doc_id: usize, url: String, method: String },
    Finished { request_id: String, doc_id: usize, status: u16, mime_type: String, headers: Vec<(String, String)>, length: usize },
    Failed { request_id: String, doc_id: usize, error: String },
}

pub struct Net {
    agent: ureq::Agent,
    pub extra_headers: Mutex<Vec<(String, String)>>,
    pub user_agent: Mutex<String>,
    events: Mutex<Option<Sender<NetEvent>>>,
    next_request: std::sync::atomic::AtomicU64,
}

impl Net {
    pub fn new(proxy: Option<&str>) -> Arc<Self> {
        let mut config = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .timeout_global(Some(Duration::from_secs(30)))
            .user_agent(USER_AGENT);
        if let Some(proxy) = proxy.and_then(|p| ureq::Proxy::new(p).ok()) {
            config = config.proxy(Some(proxy));
        }
        Arc::new(Self {
            agent: config.build().into(),
            extra_headers: Mutex::new(Vec::new()),
            user_agent: Mutex::new(USER_AGENT.to_string()),
            events: Mutex::new(None),
            next_request: std::sync::atomic::AtomicU64::new(1),
        })
    }

    pub fn set_events(&self, tx: Sender<NetEvent>) {
        *self.events.lock().unwrap() = Some(tx);
    }

    fn emit(&self, event: NetEvent) {
        if let Some(tx) = self.events.lock().unwrap().as_ref() {
            let _ = tx.send(event);
        }
    }

    pub fn request_id(&self) -> String {
        format!("ks-{}", self.next_request.fetch_add(1, std::sync::atomic::Ordering::Relaxed))
    }

    pub fn get(&self, url: &Url) -> Result<Response, String> {
        self.load(url, "GET", None, &[])
    }

    pub fn load(&self, url: &Url, method: &str, body: Option<Vec<u8>>, headers: &[(String, String)]) -> Result<Response, String> {
        match url.scheme() {
            "data" => {
                let data = data_url::DataUrl::process(url.as_str()).map_err(|e| format!("{e:?}"))?;
                let mime_type = format!("{}/{}", data.mime_type().type_, data.mime_type().subtype);
                let (body, _) = data.decode_to_vec().map_err(|e| format!("{e:?}"))?;
                Ok(Response { url: url.to_string(), status: 200, mime_type, headers: Vec::new(), body })
            }
            "file" => {
                let path = url.to_file_path().map_err(|_| format!("invalid file URL {url}"))?;
                let body = std::fs::read(&path).map_err(|e| format!("{}: {e}", path.display()))?;
                Ok(Response { url: url.to_string(), status: 200, mime_type: mime_for_path(path.to_string_lossy().as_ref()).into(), headers: Vec::new(), body })
            }
            "about" => Ok(Response { url: url.to_string(), status: 200, mime_type: "text/html".into(), headers: Vec::new(), body: Vec::new() }),
            "http" | "https" => self.http(url, method, body, headers),
            scheme => Err(format!("unsupported URL scheme {scheme}:")),
        }
    }

    fn http(&self, url: &Url, method: &str, body: Option<Vec<u8>>, headers: &[(String, String)]) -> Result<Response, String> {
        let mut builder = ureq::http::Request::builder().method(method).uri(url.as_str());
        builder = builder.header("User-Agent", self.user_agent.lock().unwrap().as_str());
        for (name, value) in self.extra_headers.lock().unwrap().iter().chain(headers) {
            builder = builder.header(name.as_str(), value.as_str());
        }
        let request = builder.body(body.unwrap_or_default()).map_err(|e| e.to_string())?;
        let mut response = self.agent.run(request).map_err(|e| e.to_string())?;
        let status = response.status().as_u16();
        let final_url = ureq::ResponseExt::get_uri(&response).to_string();
        let headers: Vec<(String, String)> = response
            .headers()
            .iter()
            .map(|(k, v)| (k.as_str().to_string(), String::from_utf8_lossy(v.as_bytes()).into_owned()))
            .collect();
        let mime_type = headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case("content-type"))
            .map(|(_, v)| v.split(';').next().unwrap_or("").trim().to_string())
            .unwrap_or_default();
        let mut body = Vec::new();
        response
            .body_mut()
            .as_reader()
            .take(MAX_BODY)
            .read_to_end(&mut body)
            .map_err(|e| e.to_string())?;
        Ok(Response { url: final_url, status, mime_type, headers, body })
    }
}

fn mime_for_path(path: &str) -> &'static str {
    let ext = path.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
    match ext.as_str() {
        "html" | "htm" => "text/html",
        "xhtml" => "application/xhtml+xml",
        "css" => "text/css",
        "js" | "mjs" => "text/javascript",
        "json" => "application/json",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "woff" => "font/woff",
        "woff2" => "font/woff2",
        "ttf" => "font/ttf",
        "otf" => "font/otf",
        _ => "text/plain",
    }
}

fn request_body(body: &Body) -> Option<Vec<u8>> {
    match body {
        Body::Bytes(bytes) => Some(bytes.to_vec()),
        Body::Form(form) => {
            let mut out = url::form_urlencoded::Serializer::new(String::new());
            for entry in form.iter() {
                out.append_pair(&entry.name, entry.value.as_ref());
            }
            Some(out.finish().into_bytes())
        }
        Body::Empty => None,
    }
}

/// Sub-resource loading for Blitz documents (stylesheets, images, fonts): one thread per request.
pub struct BlitzNet(pub Arc<Net>);

impl NetProvider for BlitzNet {
    fn fetch(&self, doc_id: usize, request: Request, handler: Box<dyn NetHandler>) {
        let net = Arc::clone(&self.0);
        std::thread::spawn(move || {
            let request_id = net.request_id();
            net.emit(NetEvent::Started { request_id: request_id.clone(), doc_id, url: request.url.to_string(), method: request.method.to_string() });
            let mut headers: Vec<(String, String)> = request
                .headers
                .iter()
                .map(|(k, v)| (k.as_str().to_string(), String::from_utf8_lossy(v.as_bytes()).into_owned()))
                .collect();
            if let Some(content_type) = &request.content_type {
                headers.push(("Content-Type".into(), content_type.clone()));
            }
            match net.load(&request.url, request.method.as_str(), request_body(&request.body), &headers) {
                Ok(response) => {
                    net.emit(NetEvent::Finished {
                        request_id,
                        doc_id,
                        status: response.status,
                        mime_type: response.mime_type.clone(),
                        headers: response.headers.clone(),
                        length: response.body.len(),
                    });
                    handler.bytes(response.url, response.body.into());
                }
                Err(error) => {
                    log::debug!("fetch {} failed: {error}", request.url);
                    net.emit(NetEvent::Failed { request_id, doc_id, error });
                }
            }
        });
    }
}

/// `<script src>` and module loading for Boa: synchronous, as classic scripts block parsing.
pub struct ScriptNet(pub Arc<Net>);

impl ScriptFetcher for ScriptNet {
    fn fetch(&self, url: &Url) -> Result<String, FetchError> {
        match self.0.get(url) {
            Ok(response) if response.status < 400 => Ok(String::from_utf8_lossy(&response.body).into_owned()),
            Ok(response) => Err(FetchError::InvalidData(format!("HTTP {}", response.status))),
            Err(error) => Err(FetchError::InvalidData(error)),
        }
    }
}

/// Link clicks and form submissions: queued for the browser loop, which navigates the page.
pub struct Navigations(pub Arc<Mutex<Vec<(usize, Url)>>>);

impl NavigationProvider for Navigations {
    fn navigate_to(&self, options: NavigationOptions) {
        self.0.lock().unwrap().push((options.source_document, options.url));
    }
}
