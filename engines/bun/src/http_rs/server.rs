//! An HTTP/1.1 server on the reactor: accept connections (TCP or a Unix socket, TLS if configured), parse requests
//! with `bun_http1`, hand each whole request to a [`Service`], write its response. A connection answers its requests
//! in order, one at a time.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::io;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::rc::{Rc, Weak};
use std::sync::Arc;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use bun_http1::request::{self, Outcome, RequestOptions, RequestParser};
use mio::Interest;
use mio::net::{TcpListener, UnixListener};
use rustls::ServerConfig;

use crate::reactor::{Io, Reactor, Readiness, Token};
use crate::transport::{Socket, Transport};

/// A request with its whole body.
#[derive(Debug, Clone)]
pub struct IncomingRequest {
    pub method: String,
    /// As the request line has it: a path and query, or an absolute URL.
    pub target: String,
    pub headers: Vec<(Vec<u8>, Vec<u8>)>,
    pub body: Vec<u8>,
    pub remote: Option<SocketAddr>,
}

impl IncomingRequest {
    pub fn header(&self, name: &[u8]) -> Option<&[u8]> {
        self.headers
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_slice())
    }
}

/// What answers requests. `respond` may be called later, from anywhere on this thread.
pub trait Service {
    fn request(&self, responder: Responder, request: IncomingRequest);
}

pub enum Bind {
    Tcp(SocketAddr),
    Unix(PathBuf),
}

enum Listener {
    Tcp(TcpListener),
    Unix(UnixListener),
}

impl mio::event::Source for Listener {
    fn register(&mut self, registry: &mio::Registry, token: Token, interests: Interest) -> io::Result<()> {
        match self {
            Listener::Tcp(l) => l.register(registry, token, interests),
            Listener::Unix(l) => l.register(registry, token, interests),
        }
    }

    fn reregister(&mut self, registry: &mio::Registry, token: Token, interests: Interest) -> io::Result<()> {
        match self {
            Listener::Tcp(l) => l.reregister(registry, token, interests),
            Listener::Unix(l) => l.reregister(registry, token, interests),
        }
    }

    fn deregister(&mut self, registry: &mio::Registry) -> io::Result<()> {
        match self {
            Listener::Tcp(l) => l.deregister(registry),
            Listener::Unix(l) => l.deregister(registry),
        }
    }
}

pub struct Server {
    reactor: Rc<Reactor>,
    listener: RefCell<Option<(Listener, Token)>>,
    local_addr: Option<SocketAddr>,
    service: Rc<dyn Service>,
    tls: Option<Arc<ServerConfig>>,
    connections: RefCell<HashMap<u64, Rc<Connection>>>,
    next_id: Cell<u64>,
    /// Whether the listening socket keeps the reactor alive.
    refed: Cell<bool>,
    in_flight: Cell<usize>,
}

impl Server {
    pub fn listen(
        reactor: &Rc<Reactor>,
        bind: Bind,
        tls: Option<Arc<ServerConfig>>,
        service: Rc<dyn Service>,
    ) -> io::Result<Rc<Server>> {
        let (mut listener, local_addr) = match bind {
            Bind::Tcp(addr) => {
                let listener = TcpListener::bind(addr)?;
                let local = listener.local_addr()?;
                (Listener::Tcp(listener), Some(local))
            }
            Bind::Unix(path) => (Listener::Unix(UnixListener::bind(path)?), None),
        };
        let server = Rc::new(Server {
            reactor: reactor.clone(),
            listener: RefCell::new(None),
            local_addr,
            service,
            tls,
            connections: RefCell::default(),
            next_id: Cell::new(0),
            refed: Cell::new(true),
            in_flight: Cell::new(0),
        });
        let accept: Rc<dyn Io> = Rc::new(Accept(Rc::downgrade(&server)));
        let token = reactor.register(&mut listener, Interest::READABLE, accept)?;
        *server.listener.borrow_mut() = Some((listener, token));
        reactor.ref_();
        Ok(server)
    }

    pub fn local_addr(&self) -> Option<SocketAddr> {
        self.local_addr
    }

    pub fn is_listening(&self) -> bool {
        self.listener.borrow().is_some()
    }

    /// Requests whose responses have not been written.
    pub fn pending_requests(&self) -> usize {
        self.in_flight.get()
    }

    /// Whether the listening socket keeps the loop alive (`server.ref()`/`unref()`).
    pub fn set_ref(&self, refed: bool) {
        if self.refed.get() == refed {
            return;
        }
        self.refed.set(refed);
        if self.is_listening() {
            if refed {
                self.reactor.ref_();
            } else {
                self.reactor.unref();
            }
        }
    }

    /// Stops accepting. `close_active` also closes the connections whose requests are in flight; idle ones are
    /// closed either way.
    pub fn stop(&self, close_active: bool) {
        if let Some((mut listener, token)) = self.listener.borrow_mut().take() {
            self.reactor.deregister(&mut listener, token);
            if self.refed.get() {
                self.reactor.unref();
            }
        }
        let connections: Vec<Rc<Connection>> = self.connections.borrow().values().cloned().collect();
        for connection in connections {
            if close_active || !connection.state.borrow().in_flight {
                connection.close();
            }
        }
    }

    fn accept(self: &Rc<Self>) {
        loop {
            let accepted = {
                let listener = self.listener.borrow();
                let Some((listener, _)) = listener.as_ref() else { return };
                match listener {
                    Listener::Tcp(l) => l.accept().map(|(s, addr)| {
                        let _ = s.set_nodelay(true);
                        (Socket::Tcp(s), Some(addr))
                    }),
                    Listener::Unix(l) => l.accept().map(|(s, _)| (Socket::Unix(s), None)),
                }
            };
            let (socket, remote) = match accepted {
                Ok(accepted) => accepted,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(_) => return,
            };
            let tls = match &self.tls {
                Some(config) => match rustls::ServerConnection::new(config.clone()) {
                    Ok(connection) => Some(connection.into()),
                    Err(_) => continue,
                },
                None => None,
            };
            let id = self.next_id.get();
            self.next_id.set(id + 1);
            let connection = Rc::new(Connection {
                id,
                server: Rc::downgrade(self),
                reactor: self.reactor.clone(),
                remote,
                state: RefCell::new(ConnectionState {
                    transport: Transport::new(socket, tls),
                    token: None,
                    parser: RequestParser::new(64 * 1024),
                    unparsed: Vec::new(),
                    current: None,
                    in_flight: false,
                    closing: false,
                    closed: false,
                }),
            });
            let io: Rc<dyn Io> = connection.clone();
            let token = {
                let mut state = connection.state.borrow_mut();
                self.reactor
                    .register(&mut state.transport.socket, Interest::READABLE | Interest::WRITABLE, io)
            };
            match token {
                Ok(token) => {
                    connection.state.borrow_mut().token = Some(token);
                    self.connections.borrow_mut().insert(id, connection);
                }
                Err(_) => continue,
            }
        }
    }
}

struct Accept(Weak<Server>);

impl Io for Accept {
    fn ready(self: Rc<Self>, _: &Rc<Reactor>, _: Readiness) {
        if let Some(server) = self.0.upgrade() {
            server.accept();
        }
    }
}

struct ConnectionState {
    transport: Transport,
    token: Option<Token>,
    parser: RequestParser,
    /// What arrived while a request was in flight, parsed once it is answered.
    unparsed: Vec<u8>,
    current: Option<IncomingRequest>,
    in_flight: bool,
    /// Close once what is queued is written.
    closing: bool,
    closed: bool,
}

struct Connection {
    id: u64,
    server: Weak<Server>,
    reactor: Rc<Reactor>,
    remote: Option<SocketAddr>,
    state: RefCell<ConnectionState>,
}

/// Where the response to one request goes.
pub struct Responder {
    connection: Rc<Connection>,
    keep_alive: bool,
    is_head: bool,
    answered: bool,
}

impl Drop for Responder {
    fn drop(&mut self) {
        if !self.answered {
            self.answered = true;
            self.connection.close();
        }
    }
}

/// The response head: status, reason phrase (the standard one if empty) and fields.
pub struct ResponseHead<'a> {
    pub status: u16,
    pub status_text: &'a str,
    pub headers: &'a [(Vec<u8>, Vec<u8>)],
}

impl Responder {
    /// Whether the client is gone.
    pub fn is_closed(&self) -> bool {
        self.connection.state.borrow().closed
    }

    pub fn remote_addr(&self) -> Option<SocketAddr> {
        self.connection.remote
    }

    /// Writes the response. `Content-Length` and `Date` are added unless `head` has them.
    pub fn respond(mut self, head: ResponseHead<'_>, body: &[u8]) {
        self.answered = true;
        let no_body = matches!(head.status, 100..=199 | 204 | 304);
        let mut out = Vec::with_capacity(256 + body.len());
        let reason = if head.status_text.is_empty() {
            reason_phrase(head.status)
        } else {
            head.status_text
        };
        out.extend_from_slice(format!("HTTP/1.1 {} {}\r\n", head.status, reason).as_bytes());
        let mut has_date = false;
        let mut closes = !self.keep_alive;
        for (name, value) in head.headers {
            // The framing is that of the body written here.
            if name.eq_ignore_ascii_case(b"content-length") || name.eq_ignore_ascii_case(b"transfer-encoding") {
                continue;
            }
            if name.eq_ignore_ascii_case(b"date") {
                has_date = true;
            }
            if name.eq_ignore_ascii_case(b"connection") && value.eq_ignore_ascii_case(b"close") {
                closes = true;
            }
            out.extend_from_slice(name);
            out.extend_from_slice(b": ");
            out.extend_from_slice(value);
            out.extend_from_slice(b"\r\n");
        }
        if !has_date {
            out.extend_from_slice(b"Date: ");
            out.extend_from_slice(http_date(SystemTime::now()).as_bytes());
            out.extend_from_slice(b"\r\n");
        }
        if closes && self.keep_alive {
            out.extend_from_slice(b"Connection: close\r\n");
        }
        if !no_body {
            out.extend_from_slice(format!("Content-Length: {}\r\n", body.len()).as_bytes());
        }
        out.extend_from_slice(b"\r\n");
        if !no_body && !self.is_head {
            out.extend_from_slice(body);
        }
        self.connection.write_response(&out, closes);
    }
}

impl Connection {
    fn write_response(self: &Rc<Self>, bytes: &[u8], closes: bool) {
        let mut state = self.state.borrow_mut();
        self.end_request(&mut state);
        if state.closed {
            return;
        }
        if state.transport.write(bytes).is_err() {
            drop(state);
            self.close();
            return;
        }
        if closes {
            state.closing = true;
        }
        let done = state.closing && !state.transport.has_pending_output();
        drop(state);
        if done {
            self.close();
            return;
        }
        // The next request of a pipeline, after this call returns.
        let next = self.clone();
        self.reactor.set_timer(Instant::now(), move |_| next.parse());
    }

    fn end_request(&self, state: &mut ConnectionState) {
        if !state.in_flight {
            return;
        }
        state.in_flight = false;
        if let Some(server) = self.server.upgrade() {
            server.in_flight.set(server.in_flight.get().saturating_sub(1));
        }
        self.reactor.unref();
    }

    fn close(&self) {
        let mut state = self.state.borrow_mut();
        if state.closed {
            return;
        }
        state.closed = true;
        self.end_request(&mut state);
        state.transport.close();
        if let Some(token) = state.token.take() {
            self.reactor.deregister(&mut state.transport.socket, token);
        }
        drop(state);
        if let Some(server) = self.server.upgrade() {
            server.connections.borrow_mut().remove(&self.id);
        }
    }

    /// Parses what has arrived, unless a request is in flight, and hands a whole request to the service.
    fn parse(self: &Rc<Self>) {
        let mut state = self.state.borrow_mut();
        if state.in_flight || state.closed || state.closing || state.unparsed.is_empty() {
            return;
        }
        let bytes = std::mem::take(&mut state.unparsed);
        let options = RequestOptions {
            max_header_size: 0,
            max_headers_count: 0,
            requires_host: false,
            has_strict_methods: false,
            has_lenient_values: false,
            has_lenient_transfer_encoding: false,
        };
        let remote = self.remote;
        let ConnectionState { parser, current, .. } = &mut *state;
        let mut complete = None;
        let collector = Collector {
            parser,
            current,
            complete: &mut complete,
            remote,
        };
        let outcome = request::read::<false>(collector, &bytes, &options);
        match outcome {
            Outcome::Consumed => {}
            Outcome::Stopped { consumed } => {
                state.unparsed = bytes.get(consumed..).unwrap_or_default().to_vec();
            }
            Outcome::Failed(error) => {
                let _ = state.transport.write(error.response());
                state.closing = true;
                let done = !state.transport.has_pending_output();
                drop(state);
                if done {
                    self.close();
                }
                return;
            }
        }
        let Some(request) = complete else { return };
        let keep_alive = !state.parser.is_closing;
        state.in_flight = true;
        drop(state);
        let Some(server) = self.server.upgrade() else {
            self.close();
            return;
        };
        server.in_flight.set(server.in_flight.get() + 1);
        self.reactor.ref_();
        let responder = Responder {
            connection: self.clone(),
            keep_alive,
            is_head: request.method == "HEAD",
            answered: false,
        };
        server.service.request(responder, request);
    }

    fn on_ready(self: &Rc<Self>, readiness: Readiness) {
        let mut state = self.state.borrow_mut();
        if state.closed {
            return;
        }
        if state.transport.flush().is_err() {
            drop(state);
            self.close();
            return;
        }
        if state.closing && !state.transport.has_pending_output() && !state.in_flight {
            drop(state);
            self.close();
            return;
        }
        let mut unparsed = std::mem::take(&mut state.unparsed);
        let read = state.transport.read(&mut unparsed);
        state.unparsed = unparsed;
        let eof = match read {
            Ok(read) => read.eof || readiness.read_closed && !readiness.readable,
            Err(_) => true,
        };
        let in_flight = state.in_flight;
        drop(state);
        self.parse();
        if eof {
            // A client may half-close after its request: answer it, then close.
            if in_flight || self.state.borrow().in_flight {
                self.state.borrow_mut().closing = true;
            } else {
                self.close();
            }
        }
    }
}

impl Io for Connection {
    fn ready(self: Rc<Self>, _: &Rc<Reactor>, readiness: Readiness) {
        self.on_ready(readiness);
    }
}

struct Collector<'a> {
    parser: &'a mut RequestParser,
    current: &'a mut Option<IncomingRequest>,
    complete: &'a mut Option<IncomingRequest>,
    remote: Option<SocketAddr>,
}

impl request::Connection for Collector<'_> {
    fn parser(&mut self) -> &mut RequestParser {
        self.parser
    }

    fn on_request(self, request: &request::Request<'_>) -> Option<Self> {
        *self.current = Some(IncomingRequest {
            method: String::from_utf8_lossy(request.method()).into_owned(),
            target: String::from_utf8_lossy(request.target()).into_owned(),
            headers: request
                .fields()
                .map(|field| (field.name.to_vec(), field.value.to_vec()))
                .collect(),
            body: Vec::new(),
            remote: self.remote,
        });
        Some(self)
    }

    fn on_body(self, data: &[u8], is_last: bool) -> Option<Self> {
        if let Some(current) = self.current.as_mut() {
            current.body.extend_from_slice(data);
        }
        if is_last {
            *self.complete = self.current.take();
            return None;
        }
        Some(self)
    }
}

pub fn reason_phrase(status: u16) -> &'static str {
    match status {
        100 => "Continue",
        101 => "Switching Protocols",
        102 => "Processing",
        103 => "Early Hints",
        200 => "OK",
        201 => "Created",
        202 => "Accepted",
        203 => "Non-Authoritative Information",
        204 => "No Content",
        205 => "Reset Content",
        206 => "Partial Content",
        207 => "Multi-Status",
        208 => "Already Reported",
        226 => "IM Used",
        300 => "Multiple Choices",
        301 => "Moved Permanently",
        302 => "Found",
        303 => "See Other",
        304 => "Not Modified",
        305 => "Use Proxy",
        307 => "Temporary Redirect",
        308 => "Permanent Redirect",
        400 => "Bad Request",
        401 => "Unauthorized",
        402 => "Payment Required",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        406 => "Not Acceptable",
        407 => "Proxy Authentication Required",
        408 => "Request Timeout",
        409 => "Conflict",
        410 => "Gone",
        411 => "Length Required",
        412 => "Precondition Failed",
        413 => "Payload Too Large",
        414 => "URI Too Long",
        415 => "Unsupported Media Type",
        416 => "Range Not Satisfiable",
        417 => "Expectation Failed",
        418 => "I'm a Teapot",
        421 => "Misdirected Request",
        422 => "Unprocessable Entity",
        423 => "Locked",
        424 => "Failed Dependency",
        425 => "Too Early",
        426 => "Upgrade Required",
        428 => "Precondition Required",
        429 => "Too Many Requests",
        431 => "Request Header Fields Too Large",
        451 => "Unavailable For Legal Reasons",
        500 => "Internal Server Error",
        501 => "Not Implemented",
        502 => "Bad Gateway",
        503 => "Service Unavailable",
        504 => "Gateway Timeout",
        505 => "HTTP Version Not Supported",
        506 => "Variant Also Negotiates",
        507 => "Insufficient Storage",
        508 => "Loop Detected",
        510 => "Not Extended",
        511 => "Network Authentication Required",
        _ => "",
    }
}

/// RFC 9110 §5.6.7 IMF-fixdate: `Sun, 06 Nov 1994 08:49:37 GMT`.
pub fn http_date(time: SystemTime) -> String {
    const DAYS: [&str; 7] = ["Thu", "Fri", "Sat", "Sun", "Mon", "Tue", "Wed"];
    const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
    let secs = time.duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs());
    let days = secs / 86_400;
    let rem = secs % 86_400;
    // Howard Hinnant's civil_from_days.
    let z = days as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{}, {:02} {} {} {:02}:{:02}:{:02} GMT",
        DAYS[(days % 7) as usize],
        day,
        MONTHS[(month - 1) as usize],
        year,
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}
