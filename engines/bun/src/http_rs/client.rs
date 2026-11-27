//! An HTTP/1.1 client exchange on the reactor: connect (TCP or a Unix socket, TLS for `https:`), send the request,
//! parse the response with `bun_http1`, follow redirects, decode the body's content coding. Each exchange has a
//! connection of its own, closed at the end.

use std::cell::RefCell;
use std::io;
use std::net::{SocketAddr, ToSocketAddrs};
use std::path::PathBuf;
use std::rc::Rc;
use std::time::Instant;

use bun_http1::{ChunkedDecoder, HeadOptions, ResponseParser};
use mio::Interest;
use mio::net::{TcpStream, UnixStream};
use url::Url;

use crate::decompress::{self, Encoding};
use crate::reactor::{Io, Reactor, Readiness, Token};
use crate::transport::{self, Socket, TlsOptions, Transport};

const MAX_REDIRECTS: u32 = 20;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Redirect {
    #[default]
    Follow,
    Manual,
    Error,
}

#[derive(Clone, Debug)]
pub struct Request {
    pub method: String,
    pub url: Url,
    /// In the order they are sent, with the case they are given in.
    pub headers: Vec<(String, Vec<u8>)>,
    pub body: Vec<u8>,
    pub redirect: Redirect,
    /// Ask for compressed bodies and decode them.
    pub decompress: bool,
    pub tls: TlsOptions,
    /// Connect to this Unix socket instead of the URL's host.
    pub unix: Option<PathBuf>,
    /// The `User-Agent` sent unless `headers` has one.
    pub user_agent: String,
    /// `Connection: keep-alive` unless `headers` says otherwise.
    pub keepalive: bool,
}

impl Request {
    pub fn get(url: Url) -> Request {
        Request {
            method: "GET".into(),
            url,
            headers: Vec::new(),
            body: Vec::new(),
            redirect: Redirect::Follow,
            decompress: true,
            tls: TlsOptions::default(),
            unix: None,
            user_agent: String::new(),
            keepalive: true,
        }
    }

    fn header(&self, name: &str) -> Option<&[u8]> {
        self.headers
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_slice())
    }

    fn remove_header(&mut self, name: &str) {
        self.headers.retain(|(n, _)| !n.eq_ignore_ascii_case(name));
    }
}

/// The status line and header fields of a response, and where it came from.
#[derive(Clone, Debug)]
pub struct Head {
    pub status: u16,
    pub status_text: Vec<u8>,
    pub headers: Vec<(Vec<u8>, Vec<u8>)>,
    /// After redirects.
    pub url: Url,
    pub redirected: bool,
}

impl Head {
    pub fn header(&self, name: &[u8]) -> Option<&[u8]> {
        self.headers
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_slice())
    }
}

#[derive(Debug)]
pub enum Error {
    UnsupportedProtocol(String),
    /// The host has no address; `code` is the resolver's (`ENOTFOUND`).
    DnsResolveFailed { host: String },
    /// `errno` of the failed connect.
    ConnectionRefused(Option<i32>),
    ConnectionClosed,
    Tls(String),
    /// What OpenSSL calls the verification error (`DEPTH_ZERO_SELF_SIGNED_CERT`), and its message.
    Certificate { code: &'static str, message: &'static str },
    MalformedResponse,
    TooManyRedirects,
    UnexpectedRedirect,
    RedirectUrlInvalid,
    Decompression(&'static str),
    Aborted,
    Io(io::Error),
}

impl Error {
    /// Bun's `code` for the error.
    pub fn code(&self) -> String {
        match self {
            Error::UnsupportedProtocol(_) => "UnsupportedProtocol".into(),
            Error::DnsResolveFailed { .. } => "ENOTFOUND".into(),
            Error::ConnectionRefused(errno) => errno
                .and_then(errno_name)
                .unwrap_or("ECONNREFUSED")
                .into(),
            Error::ConnectionClosed => "ECONNRESET".into(),
            Error::Tls(_) => "EPROTO".into(),
            Error::Certificate { code, .. } => (*code).into(),
            Error::MalformedResponse => "Malformed_HTTP_Response".into(),
            Error::TooManyRedirects => "TooManyRedirects".into(),
            Error::UnexpectedRedirect => "UnexpectedRedirect".into(),
            Error::RedirectUrlInvalid => "RedirectURLInvalid".into(),
            Error::Decompression(_) => "ZlibError".into(),
            Error::Aborted => "AbortError".into(),
            Error::Io(error) => error
                .raw_os_error()
                .and_then(errno_name)
                .unwrap_or("ECONNRESET")
                .into(),
        }
    }

    /// Bun's message, for the request to `href`.
    pub fn message(&self, href: &str) -> String {
        let code = self.code();
        match self {
            Error::UnsupportedProtocol(protocol) => {
                format!("protocol must be http:, https: or s3:, got {protocol}")
            }
            Error::DnsResolveFailed { host } => format!("getaddrinfo ENOTFOUND {host}"),
            Error::ConnectionClosed => format!(
                "{code}: The socket connection was closed unexpectedly. For more information, pass `verbose: true` in the second argument to fetch()"
            ),
            Error::ConnectionRefused(_) => {
                format!("{code}: Unable to connect. Is the computer able to access the url?")
            }
            Error::Tls(_) => format!(
                "{code}: The TLS handshake failed. Does the server speak TLS on this port?"
            ),
            Error::Certificate { message, .. } => (*message).into(),
            Error::TooManyRedirects => format!(
                "{code}: The response redirected too many times. For more information, pass `verbose: true` in the second argument to fetch()"
            ),
            Error::RedirectUrlInvalid => {
                format!("{code}: Redirect URL in Location header is invalid.")
            }
            Error::Decompression(message) => (*message).into(),
            Error::Aborted => "The operation was aborted.".into(),
            _ => format!(
                "{code} fetching \"{href}\". For more information, pass `verbose: true` in the second argument to fetch()"
            ),
        }
    }
}

fn errno_name(errno: i32) -> Option<&'static str> {
    Some(match errno {
        1 => "EPERM",
        2 => "ENOENT",
        13 => "EACCES",
        32 => "EPIPE",
        98 => "EADDRINUSE",
        99 => "EADDRNOTAVAIL",
        101 => "ENETUNREACH",
        103 => "ECONNABORTED",
        104 => "ECONNRESET",
        110 => "ETIMEDOUT",
        111 => "ECONNREFUSED",
        113 => "EHOSTUNREACH",
        _ => return None,
    })
}

fn certificate_error(error: &io::Error) -> Option<Error> {
    let tls = error.get_ref()?.downcast_ref::<rustls::Error>()?;
    let rustls::Error::InvalidCertificate(certificate) = tls else {
        return None;
    };
    use rustls::CertificateError as E;
    let (code, message) = match certificate {
        E::Expired | E::ExpiredContext { .. } => ("CERT_HAS_EXPIRED", "certificate has expired"),
        E::NotValidYet | E::NotValidYetContext { .. } => {
            ("CERT_NOT_YET_VALID", "certificate is not yet valid")
        }
        E::Revoked => ("CERT_REVOKED", "certificate revoked"),
        E::NotValidForName | E::NotValidForNameContext { .. } => (
            "ERR_TLS_CERT_ALTNAME_INVALID",
            "Hostname/IP does not match certificate's altnames",
        ),
        E::UnknownIssuer => ("DEPTH_ZERO_SELF_SIGNED_CERT", "self signed certificate"),
        _ => (
            "UNABLE_TO_VERIFY_LEAF_SIGNATURE",
            "unable to verify the first certificate",
        ),
    };
    Some(Error::Certificate { code, message })
}

/// What an exchange tells its owner, in this order: `head` once (unless it fails first), `body` for each part of
/// the decoded body, `end` once.
pub trait Events {
    fn head(&self, head: Head);
    fn body(&self, chunk: &[u8]);
    fn end(&self, result: Result<(), Error>);
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Phase {
    Connecting,
    Head,
    Body,
    Done,
}

enum Framing {
    None,
    Length(u64),
    Chunked(ChunkedDecoder),
    UntilClose,
}

struct Connection {
    transport: Transport,
    token: Token,
}

struct State {
    request: Request,
    addresses: Vec<SocketAddr>,
    redirects: u32,
    redirected: bool,
    connection: Option<Connection>,
    phase: Phase,
    input: Vec<u8>,
    parser: ResponseParser,
    framing: Framing,
    encoding: Option<Encoding>,
    compressed: Vec<u8>,
}

struct Exchange {
    reactor: Rc<Reactor>,
    state: RefCell<State>,
    events: Rc<dyn Events>,
}

/// An exchange in flight.
#[derive(Clone)]
pub struct Handle(Rc<Exchange>);

impl Handle {
    /// Ends the exchange with [`Error::Aborted`], unless it has ended.
    pub fn abort(&self) {
        self.0.fail(Error::Aborted);
    }
}

/// Starts `request`. The reactor stays alive until it ends.
pub fn start(reactor: &Rc<Reactor>, request: Request, events: Rc<dyn Events>) -> Handle {
    let exchange = Rc::new(Exchange {
        reactor: reactor.clone(),
        state: RefCell::new(State {
            request,
            addresses: Vec::new(),
            redirects: 0,
            redirected: false,
            connection: None,
            phase: Phase::Connecting,
            input: Vec::new(),
            parser: ResponseParser::default(),
            framing: Framing::None,
            encoding: None,
            compressed: Vec::new(),
        }),
        events,
    });
    reactor.ref_();
    let started = exchange.clone();
    // Not within `start`: the owner's state is not set up yet.
    reactor.set_timer(Instant::now(), move |_| {
        if let Err(error) = started.resolve_and_connect() {
            started.fail(error);
        }
    });
    Handle(exchange)
}

/// The response of `request` with its whole body, for a caller without JavaScript.
pub async fn fetch(reactor: &Rc<Reactor>, request: Request) -> Result<(Head, Vec<u8>), Error> {
    #[derive(Default)]
    struct Collect {
        head: RefCell<Option<Head>>,
        body: RefCell<Vec<u8>>,
        end: RefCell<Option<Result<(), Error>>>,
        waker: RefCell<Option<std::task::Waker>>,
    }
    impl Events for Collect {
        fn head(&self, head: Head) {
            *self.head.borrow_mut() = Some(head);
        }
        fn body(&self, chunk: &[u8]) {
            self.body.borrow_mut().extend_from_slice(chunk);
        }
        fn end(&self, result: Result<(), Error>) {
            *self.end.borrow_mut() = Some(result);
            if let Some(waker) = self.waker.borrow_mut().take() {
                waker.wake();
            }
        }
    }
    let collect = Rc::new(Collect::default());
    let _handle = start(reactor, request, collect.clone());
    std::future::poll_fn(|cx| {
        let Some(result) = collect.end.borrow_mut().take() else {
            *collect.waker.borrow_mut() = Some(cx.waker().clone());
            return std::task::Poll::Pending;
        };
        std::task::Poll::Ready(result.and_then(|()| {
            let head = collect.head.borrow_mut().take().ok_or(Error::ConnectionClosed)?;
            Ok((head, std::mem::take(&mut *collect.body.borrow_mut())))
        }))
    })
    .await
}

fn has_request_body(method: &str) -> bool {
    matches!(method, "POST" | "PUT" | "PATCH")
}

fn base64(input: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
    for chunk in input.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = u32::from(b[0]) << 16 | u32::from(b[1]) << 8 | u32::from(b[2]);
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(ALPHABET[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// The request's bytes on the wire, as Bun's client writes them: the given fields, then the defaults it lacks.
pub fn serialize(request: &Request) -> Vec<u8> {
    let url = &request.url;
    let mut out = Vec::with_capacity(256 + request.body.len());
    out.extend_from_slice(request.method.as_bytes());
    out.push(b' ');
    out.extend_from_slice(url.path().as_bytes());
    if let Some(query) = url.query() {
        out.push(b'?');
        out.extend_from_slice(query.as_bytes());
    }
    out.extend_from_slice(b" HTTP/1.1\r\n");
    let mut field = |name: &[u8], value: &[u8]| {
        out.extend_from_slice(name);
        out.extend_from_slice(b": ");
        out.extend_from_slice(value);
        out.extend_from_slice(b"\r\n");
    };
    for (name, value) in &request.headers {
        if name.eq_ignore_ascii_case("content-length") || name.eq_ignore_ascii_case("transfer-encoding") {
            continue;
        }
        field(name.as_bytes(), value);
    }
    if request.header("connection").is_none() && request.keepalive {
        field(b"Connection", b"keep-alive");
    }
    if request.header("user-agent").is_none() {
        field(b"User-Agent", request.user_agent.as_bytes());
    }
    if request.header("accept").is_none() {
        field(b"Accept", b"*/*");
    }
    if request.header("host").is_none() {
        let mut host = url.host_str().unwrap_or("").to_owned();
        if let Some(port) = url.port() {
            host = format!("{host}:{port}");
        }
        field(b"Host", host.as_bytes());
    }
    if request.header("accept-encoding").is_none() && request.decompress {
        field(b"Accept-Encoding", b"gzip, deflate, br, zstd");
    }
    if request.header("authorization").is_none() && !url.username().is_empty() {
        let user = percent_decode(url.username());
        let password = percent_decode(url.password().unwrap_or(""));
        let credentials = [user.as_slice(), b":", password.as_slice()].concat();
        field(b"Authorization", format!("Basic {}", base64(&credentials)).as_bytes());
    }
    if !request.body.is_empty() || has_request_body(&request.method) {
        field(b"Content-Length", request.body.len().to_string().as_bytes());
    }
    out.extend_from_slice(b"\r\n");
    out.extend_from_slice(&request.body);
    out
}

fn percent_decode(input: &str) -> Vec<u8> {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let hex = |b: u8| (b as char).to_digit(16);
        match (bytes[i], bytes.get(i + 1).copied().and_then(hex), bytes.get(i + 2).copied().and_then(hex)) {
            (b'%', Some(high), Some(low)) => {
                out.push((high * 16 + low) as u8);
                i += 3;
            }
            (byte, _, _) => {
                out.push(byte);
                i += 1;
            }
        }
    }
    out
}

impl Exchange {
    fn resolve_and_connect(self: &Rc<Self>) -> Result<(), Error> {
        let mut state = self.state.borrow_mut();
        let url = state.request.url.clone();
        if !matches!(url.scheme(), "http" | "https") {
            return Err(Error::UnsupportedProtocol(format!("{}:", url.scheme())));
        }
        if state.request.unix.is_some() {
            state.addresses.clear();
        } else {
            let host = url.host_str().unwrap_or("");
            let port = url.port_or_known_default().unwrap_or(80);
            let bare = host.trim_start_matches('[').trim_end_matches(']');
            let mut addresses: Vec<SocketAddr> = (bare, port)
                .to_socket_addrs()
                .map_err(|_| Error::DnsResolveFailed { host: host.to_owned() })?
                .collect();
            if addresses.is_empty() {
                return Err(Error::DnsResolveFailed { host: host.to_owned() });
            }
            // IPv4 first, which is where most local servers listen.
            addresses.sort_by_key(|a| a.is_ipv6());
            addresses.reverse();
            state.addresses = addresses;
        }
        drop(state);
        self.connect_next()
    }

    /// Connects to the next address (the last one first), or to the Unix socket.
    fn connect_next(self: &Rc<Self>) -> Result<(), Error> {
        let mut state = self.state.borrow_mut();
        let socket = match state.request.unix.clone() {
            Some(path) => Socket::Unix(UnixStream::connect(&path).map_err(|e| Error::ConnectionRefused(e.raw_os_error()))?),
            None => {
                let address = state.addresses.pop().ok_or(Error::ConnectionRefused(None))?;
                Socket::Tcp(TcpStream::connect(address).map_err(|e| Error::ConnectionRefused(e.raw_os_error()))?)
            }
        };
        let tls = if state.request.url.scheme() == "https" {
            let host = state.request.url.host_str().unwrap_or("").to_owned();
            Some(transport::client_connection(&state.request.tls, &host).map_err(|e| Error::Tls(e.to_string()))?)
        } else {
            None
        };
        let mut transport = Transport::new(socket, tls);
        let io: Rc<dyn Io> = self.clone();
        let token = self
            .reactor
            .register(&mut transport.socket, Interest::READABLE | Interest::WRITABLE, io)
            .map_err(Error::Io)?;
        state.connection = Some(Connection { transport, token });
        state.phase = Phase::Connecting;
        state.input.clear();
        state.parser = ResponseParser::default();
        Ok(())
    }

    fn close(&self, state: &mut State) {
        if let Some(mut connection) = state.connection.take() {
            self.reactor.deregister(&mut connection.transport.socket, connection.token);
        }
    }

    fn fail(&self, error: Error) {
        let mut state = self.state.borrow_mut();
        if state.phase == Phase::Done {
            return;
        }
        state.phase = Phase::Done;
        self.close(&mut state);
        drop(state);
        self.reactor.unref();
        self.events.end(Err(error));
    }

    fn succeed(&self, state: &mut State) {
        state.phase = Phase::Done;
        self.close(state);
        self.reactor.unref();
    }

    fn on_ready(self: &Rc<Self>, readiness: Readiness) -> Result<(), Error> {
        let mut state = self.state.borrow_mut();
        if state.phase == Phase::Done {
            return Ok(());
        }
        if state.phase == Phase::Connecting {
            let connection = state.connection.as_mut().ok_or(Error::ConnectionClosed)?;
            let socket = &connection.transport.socket;
            let failed = match socket.take_error() {
                Ok(Some(error)) | Err(error) => Some(error),
                Ok(None) if readiness.error || readiness.write_closed => {
                    Some(io::Error::from(io::ErrorKind::ConnectionRefused))
                }
                Ok(None) => None,
            };
            if let Some(error) = failed {
                self.close(&mut state);
                let more = !state.addresses.is_empty();
                drop(state);
                if more {
                    return self.connect_next();
                }
                return Err(Error::ConnectionRefused(error.raw_os_error()));
            }
            if !socket.is_connected() {
                return Ok(());
            }
            state.phase = Phase::Head;
            let bytes = serialize(&state.request);
            let connection = state.connection.as_mut().ok_or(Error::ConnectionClosed)?;
            connection.transport.write(&bytes).map_err(|e| self.io_error(e))?;
        }
        let connection = state.connection.as_mut().ok_or(Error::ConnectionClosed)?;
        connection.transport.flush().map_err(|e| self.io_error(e))?;
        let mut input = std::mem::take(&mut state.input);
        let read = state
            .connection
            .as_mut()
            .ok_or(Error::ConnectionClosed)?
            .transport
            .read(&mut input)
            .map_err(|e| self.io_error(e));
        state.input = input;
        let eof = read?.eof || (readiness.read_closed && !readiness.readable);
        drop(state);
        self.process()?;
        if eof {
            self.on_eof()?;
        }
        Ok(())
    }

    fn io_error(&self, error: io::Error) -> Error {
        if let Some(error) = certificate_error(&error) {
            return error;
        }
        if error.kind() == io::ErrorKind::InvalidData {
            return Error::Tls(error.to_string());
        }
        match error.kind() {
            io::ErrorKind::ConnectionReset | io::ErrorKind::BrokenPipe | io::ErrorKind::UnexpectedEof => {
                Error::ConnectionClosed
            }
            _ => Error::Io(error),
        }
    }

    fn process(self: &Rc<Self>) -> Result<(), Error> {
        let mut state = self.state.borrow_mut();
        if state.phase == Phase::Head {
            let options = HeadOptions {
                max_len: 1024 * 1024,
                max_headers: 512,
                max_interim_len: 1024 * 1024,
                spare_capacity: 0,
            };
            let input = std::mem::take(&mut state.input);
            let parsed = state.parser.feed(&input, options).map_err(|_| Error::MalformedResponse)?;
            let Some((response, rest)) = parsed else {
                return Ok(());
            };
            let rest = rest.to_vec();
            let head = Head {
                status: response.status_code as u16,
                status_text: response.status_text().to_vec(),
                headers: response
                    .headers
                    .iter()
                    .map(|h| (h.name().to_vec(), h.value().to_vec()))
                    .collect(),
                url: state.request.url.clone(),
                redirected: state.redirected,
            };
            state.input = rest;
            if let Some(next) = self.redirect(&mut state, &head)? {
                state.request = next;
                state.redirects += 1;
                state.redirected = true;
                self.close(&mut state);
                drop(state);
                return self.resolve_and_connect();
            }
            let no_body = state.request.method == "HEAD"
                || matches!(head.status, 100..=199 | 204 | 304);
            state.framing = if no_body {
                Framing::None
            } else if head
                .header(b"transfer-encoding")
                .is_some_and(|v| v.to_ascii_lowercase().ends_with(b"chunked"))
            {
                Framing::Chunked(ChunkedDecoder::default())
            } else if let Some(length) = head.header(b"content-length") {
                let length = std::str::from_utf8(length)
                    .ok()
                    .and_then(|s| s.trim().parse::<u64>().ok())
                    .ok_or(Error::MalformedResponse)?;
                Framing::Length(length)
            } else {
                Framing::UntilClose
            };
            state.encoding = if state.request.decompress && !no_body {
                head.header(b"content-encoding").and_then(Encoding::parse)
            } else {
                None
            };
            state.phase = Phase::Body;
            drop(state);
            self.events.head(head);
            state = self.state.borrow_mut();
            if state.phase == Phase::Done {
                return Ok(());
            }
        }
        if state.phase != Phase::Body {
            return Ok(());
        }
        let input = std::mem::take(&mut state.input);
        let (data, finished): (Vec<u8>, bool) = match &mut state.framing {
            Framing::None => (Vec::new(), true),
            Framing::Length(remaining) => {
                let take = (*remaining).min(input.len() as u64) as usize;
                *remaining -= take as u64;
                (input[..take].to_vec(), *remaining == 0)
            }
            Framing::Chunked(decoder) => {
                let mut out = Vec::new();
                let done = decoder.decode(&input, &mut out).map_err(|_| Error::MalformedResponse)?;
                (out, done.is_some())
            }
            Framing::UntilClose => (input, false),
        };
        self.deliver(state, data, finished)
    }

    /// Hands `data` to the owner, decoded, without holding the state: the owner may abort or start another exchange.
    fn deliver(&self, mut state: std::cell::RefMut<'_, State>, data: Vec<u8>, finished: bool) -> Result<(), Error> {
        let decoded = match state.encoding {
            Some(encoding) => {
                state.compressed.extend_from_slice(&data);
                if !finished {
                    return Ok(());
                }
                let compressed = std::mem::take(&mut state.compressed);
                if compressed.is_empty() {
                    Vec::new()
                } else {
                    decompress::decode(encoding, &compressed).map_err(Error::Decompression)?
                }
            }
            None => data,
        };
        if finished {
            self.succeed(&mut state);
        }
        drop(state);
        if !decoded.is_empty() {
            self.events.body(&decoded);
        }
        if finished {
            self.events.end(Ok(()));
        }
        Ok(())
    }

    fn on_eof(self: &Rc<Self>) -> Result<(), Error> {
        let state = self.state.borrow_mut();
        match (state.phase, &state.framing) {
            (Phase::Done, _) => Ok(()),
            (Phase::Body, Framing::UntilClose) => self.deliver(state, Vec::new(), true),
            _ => Err(Error::ConnectionClosed),
        }
    }

    /// The next request, if `head` redirects and the request follows it.
    fn redirect(&self, state: &mut State, head: &Head) -> Result<Option<Request>, Error> {
        if !matches!(head.status, 301 | 302 | 303 | 307 | 308) {
            return Ok(None);
        }
        let Some(location) = head.header(b"location") else {
            return Ok(None);
        };
        match state.request.redirect {
            Redirect::Manual => return Ok(None),
            Redirect::Error => return Err(Error::UnexpectedRedirect),
            Redirect::Follow => {}
        }
        if state.redirects >= MAX_REDIRECTS {
            return Err(Error::TooManyRedirects);
        }
        let location = String::from_utf8_lossy(location);
        let url = state.request.url.join(&location).map_err(|_| Error::RedirectUrlInvalid)?;
        let mut next = state.request.clone();
        let drops_body = head.status == 303 && next.method != "HEAD"
            || matches!(head.status, 301 | 302) && next.method == "POST";
        if drops_body {
            next.method = "GET".into();
            next.body.clear();
            for name in ["content-length", "content-type", "content-encoding", "content-language", "content-location"] {
                next.remove_header(name);
            }
        }
        if url.origin() != next.url.origin() {
            for name in ["authorization", "proxy-authorization", "cookie"] {
                next.remove_header(name);
            }
        }
        next.remove_header("host");
        next.url = url;
        Ok(Some(next))
    }
}

impl Io for Exchange {
    fn ready(self: Rc<Self>, _: &Rc<Reactor>, readiness: Readiness) {
        if let Err(error) = self.on_ready(readiness) {
            self.fail(error);
        }
    }
}
