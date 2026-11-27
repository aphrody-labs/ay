//! Loopback servers and clients through the `us_*` C ABI, as `bun_uws_sys` calls it. The test binary is the owner:
//! it defines the `us_dispatch_*` that the loop calls, and keys its behavior on the socket's kind.
#![feature(linkage)]
#![allow(clippy::missing_safety_doc)]

use core::ffi::{c_char, c_int, c_uint, c_void};
use core::ptr;
use std::cell::RefCell;
use std::ffi::CString;

use bun_usockets_rs as _;

type Socket = c_void;
type Loop = c_void;
type Group = c_void;

#[repr(C)]
#[derive(Clone, Copy)]
struct VerifyError {
    error: c_int,
    code: *const c_char,
    reason: *const c_char,
}

unsafe extern "C" {
    fn us_create_loop(
        hint: *mut c_void,
        wakeup: Option<unsafe extern "C" fn(*mut Loop)>,
        pre: Option<unsafe extern "C" fn(*mut Loop)>,
        post: Option<unsafe extern "C" fn(*mut Loop)>,
        ext: c_uint,
    ) -> *mut Loop;
    fn us_loop_run_bun_tick(loop_: *mut Loop, timeout: *const libc::timespec, now_ns: u64);
    fn us_socket_group_init(group: *mut Group, loop_: *mut Loop, vtable: *const c_void, ext: *mut c_void);
    fn us_socket_group_close_all(group: *mut Group);
    fn us_socket_group_listen(
        group: *mut Group,
        kind: u8,
        ssl_ctx: *mut c_void,
        host: *const c_char,
        port: c_int,
        options: c_int,
        ext: c_int,
        error: *mut c_int,
    ) -> *mut c_void;
    fn us_socket_group_listen_unix(
        group: *mut Group,
        kind: u8,
        ssl_ctx: *mut c_void,
        path: *const c_char,
        len: usize,
        options: c_int,
        ext: c_int,
        error: *mut c_int,
    ) -> *mut c_void;
    fn us_listen_socket_port(ls: *mut c_void) -> c_int;
    fn us_listen_socket_close(ls: *mut c_void);
    fn us_socket_group_connect(
        group: *mut Group,
        kind: u8,
        ssl_ctx: *mut c_void,
        host: *const c_char,
        port: c_int,
        local_host: *const c_char,
        local_port: c_int,
        options: c_int,
        ext: c_int,
        has_dns_resolved: *mut c_int,
    ) -> *mut c_void;
    fn us_socket_group_connect_unix(
        group: *mut Group,
        kind: u8,
        ssl_ctx: *mut c_void,
        path: *const c_char,
        len: usize,
        options: c_int,
        ext: c_int,
    ) -> *mut Socket;
    fn us_socket_from_fd(group: *mut Group, kind: u8, ssl_ctx: *mut c_void, ext: c_int, fd: c_int, options: c_int, ipc: c_int) -> *mut Socket;
    fn us_socket_adopt(s: *mut Socket, group: *mut Group, kind: u8, old_ext: c_int, ext: c_int) -> *mut Socket;
    fn us_socket_ipc_write_fd(s: *mut Socket, data: *const c_char, len: c_int, fd: c_int) -> c_int;
    fn us_socket_write(s: *mut Socket, data: *const c_char, len: c_int) -> c_int;
    fn us_socket_close(s: *mut Socket, code: c_int, reason: *mut c_void) -> *mut Socket;
    fn us_socket_kind(s: *mut Socket) -> u8;
    fn us_socket_ext(s: *mut Socket) -> *mut c_void;
    fn us_socket_get_native_handle(s: *mut Socket) -> *mut c_void;

    fn us_ssl_ctx_from_options(options: bun_uws_sys::BunSocketContextOptions, err: *mut c_int) -> *mut c_void;
    fn SSL_CTX_free(ctx: *mut c_void);
    fn SSL_get_SSL_CTX(ssl: *mut c_void) -> *mut c_void;
    fn SSL_set_tlsext_host_name(ssl: *mut c_void, name: *const c_char) -> c_int;
    fn SSL_get_servername(ssl: *mut c_void, kind: c_int) -> *const c_char;
    fn SSL_session_reused(ssl: *mut c_void) -> c_int;
    fn us_listen_socket_add_server_name(ls: *mut c_void, pattern: *const c_char, ctx: *mut c_void);
}

const SERVER: u8 = 100;
const CLIENT: u8 = 101;
const ADOPTED: u8 = 102;
const IPC: u8 = 103;
const TLS_SERVER: u8 = 110;
const TLS_CLIENT: u8 = 111;

#[derive(Debug, Clone, PartialEq)]
enum Event {
    Open(u8, bool),
    Data(u8, Vec<u8>),
    Fd(u8),
    Close(u8),
    ConnectError(c_int),
    ConnectingError(c_int),
    End(u8),
    /// The kind, whether it succeeded, the verify error, and whether the session was resumed.
    Handshake(u8, bool, c_int, bool),
}

thread_local! {
    static EVENTS: RefCell<Vec<Event>> = const { RefCell::new(Vec::new()) };
    /// Where an accepted socket is adopted to, if anywhere.
    static ADOPT_INTO: RefCell<Option<usize>> = const { RefCell::new(None) };
    static ADOPTED_EXT: RefCell<Option<usize>> = const { RefCell::new(None) };
    /// The SNI that TLS clients send.
    static SNI: RefCell<Option<CString>> = const { RefCell::new(None) };
    /// What the last TLS server's handshake saw: the client's name, and its SSL's context.
    static SERVER_SAW: RefCell<Option<(Option<String>, usize)>> = const { RefCell::new(None) };
}

fn record(event: Event) {
    EVENTS.with(|e| e.borrow_mut().push(event));
}

fn events() -> Vec<Event> {
    EVENTS.with(|e| e.borrow().clone())
}

fn write(s: *mut Socket, data: &[u8]) {
    // SAFETY: a live socket.
    let n = unsafe { us_socket_write(s, data.as_ptr().cast(), data.len() as c_int) };
    assert_eq!(n as usize, data.len());
}

#[unsafe(no_mangle)]
unsafe extern "C" fn us_dispatch_open(s: *mut Socket, is_client: c_int, _ip: *mut c_char, _len: c_int) -> *mut Socket {
    // SAFETY: the loop's live socket.
    let kind = unsafe { us_socket_kind(s) };
    record(Event::Open(kind, is_client != 0));
    if kind == CLIENT {
        write(s, b"hello");
    }
    if kind == TLS_CLIENT
        && let Some(name) = SNI.with(|n| n.borrow().clone())
    {
        // SAFETY: a TLS socket's SSL.
        unsafe { SSL_set_tlsext_host_name(us_socket_get_native_handle(s), name.as_ptr()) };
    }
    if kind == SERVER
        && let Some(group) = ADOPT_INTO.with(|a| *a.borrow())
    {
        // SAFETY: as above; the new block replaces `s`.
        let adopted = unsafe { us_socket_adopt(s, group as *mut Group, ADOPTED, 0, 64) };
        assert!(!adopted.is_null() && adopted != s);
        // SAFETY: the ext of the adopted socket, 64 bytes.
        unsafe { us_socket_ext(adopted).cast::<u64>().write(0xADD0) };
        ADOPTED_EXT.with(|e| *e.borrow_mut() = Some(adopted as usize));
        return adopted;
    }
    s
}

#[unsafe(no_mangle)]
unsafe extern "C" fn us_dispatch_data(s: *mut Socket, data: *mut c_char, len: c_int) -> *mut Socket {
    // SAFETY: the loop's live socket and its buffer.
    let (kind, data) = unsafe { (us_socket_kind(s), core::slice::from_raw_parts(data.cast::<u8>(), len as usize).to_vec()) };
    record(Event::Data(kind, data.clone()));
    match kind {
        SERVER | ADOPTED | TLS_SERVER => write(s, &[b"echo:".as_slice(), &data].concat()),
        // SAFETY: as above.
        CLIENT | TLS_CLIENT => unsafe {
            us_socket_close(s, 0, ptr::null_mut());
        },
        _ => {}
    }
    if kind == ADOPTED {
        // SAFETY: the adopted socket's ext.
        assert_eq!(unsafe { us_socket_ext(s).cast::<u64>().read() }, 0xADD0);
    }
    s
}

#[unsafe(no_mangle)]
unsafe extern "C" fn us_dispatch_fd(s: *mut Socket, fd: c_int) -> *mut Socket {
    // SAFETY: the loop's live socket; `fd` is ours.
    unsafe {
        let mut st: libc::stat = core::mem::zeroed();
        assert_eq!(libc::fstat(fd, &mut st), 0, "a received fd is open");
        libc::close(fd);
        record(Event::Fd(us_socket_kind(s)));
    }
    s
}

#[unsafe(no_mangle)]
extern "C" fn us_dispatch_writable(s: *mut Socket) -> *mut Socket {
    s
}

#[unsafe(no_mangle)]
unsafe extern "C" fn us_dispatch_close(s: *mut Socket, _code: c_int, _reason: *mut c_void) -> *mut Socket {
    // SAFETY: a socket that just closed is still readable.
    record(Event::Close(unsafe { us_socket_kind(s) }));
    s
}

#[unsafe(no_mangle)]
extern "C" fn us_dispatch_timeout(s: *mut Socket) -> *mut Socket {
    s
}

#[unsafe(no_mangle)]
extern "C" fn us_dispatch_long_timeout(s: *mut Socket) -> *mut Socket {
    s
}

#[unsafe(no_mangle)]
unsafe extern "C" fn us_dispatch_end(s: *mut Socket) -> *mut Socket {
    // SAFETY: the loop's live socket.
    record(Event::End(unsafe { us_socket_kind(s) }));
    s
}

#[unsafe(no_mangle)]
extern "C" fn us_dispatch_connect_error(s: *mut Socket, code: c_int) -> *mut Socket {
    record(Event::ConnectError(code));
    s
}

#[unsafe(no_mangle)]
extern "C" fn us_dispatch_connecting_error(c: *mut c_void, code: c_int) -> *mut c_void {
    record(Event::ConnectingError(code));
    c
}

#[unsafe(no_mangle)]
unsafe extern "C" fn us_dispatch_handshake(s: *mut Socket, ok: c_int, err: VerifyError) {
    // SAFETY: the loop's live TLS socket and its SSL.
    unsafe {
        let kind = us_socket_kind(s);
        let ssl = us_socket_get_native_handle(s);
        record(Event::Handshake(kind, ok != 0, err.error, SSL_session_reused(ssl) != 0));
        if kind == TLS_SERVER {
            let name = SSL_get_servername(ssl, 0);
            let name = (!name.is_null()).then(|| core::ffi::CStr::from_ptr(name).to_string_lossy().into_owned());
            SERVER_SAW.with(|saw| *saw.borrow_mut() = Some((name, SSL_get_SSL_CTX(ssl) as usize)));
        }
        if kind == TLS_CLIENT && ok != 0 {
            write(s, b"hello");
        }
    }
}

/// The owner of adopted TLS server sockets: none here.
#[unsafe(no_mangle)]
extern "C" fn us_dispatch_socket_server_name(_s: *mut Socket, _name: *const c_char, _abort: *mut c_int) -> *mut c_void {
    ptr::null_mut()
}

#[unsafe(no_mangle)]
#[linkage = "weak"]
extern "C" fn Bun__internal_dispatch_ready_poll(_loop: *mut Loop, _poll: *mut c_void) {
    unreachable!("no bun_io polls in these tests")
}

unsafe extern "C" fn noop(_: *mut Loop) {}

struct Harness {
    loop_: *mut Loop,
    server: *mut Group,
    client: *mut Group,
}

/// `us_socket_group_t` is 9 pointers and 16 bytes.
fn new_group(loop_: *mut Loop) -> *mut Group {
    let group = Box::into_raw(Box::new([0u64; 11])).cast::<Group>();
    // SAFETY: room for a group.
    unsafe { us_socket_group_init(group, loop_, ptr::null(), ptr::null_mut()) };
    group
}

impl Harness {
    fn new() -> Self {
        EVENTS.with(|e| e.borrow_mut().clear());
        // SAFETY: callbacks that do nothing.
        let loop_ = unsafe { us_create_loop(ptr::null_mut(), Some(noop), Some(noop), Some(noop), 0) };
        assert!(!loop_.is_null());
        Harness { loop_, server: new_group(loop_), client: new_group(loop_) }
    }

    /// Ticks until `done` says so, for at most five seconds.
    fn run_until(&self, done: impl Fn(&[Event]) -> bool) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let timeout = libc::timespec { tv_sec: 0, tv_nsec: 10_000_000 };
        while !done(&events()) {
            assert!(std::time::Instant::now() < deadline, "timed out with {:?}", events());
            // SAFETY: this thread's loop.
            unsafe { us_loop_run_bun_tick(self.loop_, &timeout, 0) };
        }
    }

    fn close_all(&self) {
        // SAFETY: the groups of this loop.
        unsafe {
            us_socket_group_close_all(self.client);
            us_socket_group_close_all(self.server);
        }
    }
}

fn received_echo(events: &[Event]) -> bool {
    events.contains(&Event::Data(CLIENT, b"echo:hello".to_vec())) && events.contains(&Event::Close(CLIENT))
}

fn listen_tcp(h: &Harness) -> (*mut c_void, c_int) {
    let mut error = 0;
    // SAFETY: the harness's group.
    let ls = unsafe { us_socket_group_listen(h.server, SERVER, ptr::null_mut(), c"127.0.0.1".as_ptr(), 0, 0, 0, &mut error) };
    assert!(!ls.is_null(), "listen: errno {error}");
    // SAFETY: a live listener.
    (ls, unsafe { us_listen_socket_port(ls) })
}

fn connect(h: &Harness, host: &std::ffi::CStr, port: c_int) -> c_int {
    let mut resolved = 0;
    // SAFETY: the harness's group.
    let c = unsafe {
        us_socket_group_connect(h.client, CLIENT, ptr::null_mut(), host.as_ptr(), port, ptr::null(), 0, 0, 0, &mut resolved)
    };
    assert!(!c.is_null());
    resolved
}

#[test]
fn tcp_listen_accept_and_echo() {
    let h = Harness::new();
    let (ls, port) = listen_tcp(&h);
    assert!(port > 0);
    assert_eq!(connect(&h, c"127.0.0.1", port), 1);
    h.run_until(received_echo);
    let events = events();
    assert!(events.contains(&Event::Open(SERVER, false)), "{events:?}");
    assert!(events.contains(&Event::Open(CLIENT, true)), "{events:?}");
    assert!(events.contains(&Event::Data(SERVER, b"hello".to_vec())), "{events:?}");
    // The client's close reaches the server as the end of its stream.
    h.run_until(|e| e.contains(&Event::Close(SERVER)));
    assert!(self::events().contains(&Event::End(SERVER)));
    // SAFETY: the live listener.
    unsafe { us_listen_socket_close(ls) };
    h.close_all();
}

#[test]
fn a_name_goes_through_the_resolver() {
    let h = Harness::new();
    let (_ls, port) = listen_tcp(&h);
    assert_eq!(connect(&h, c"localhost", port), 0, "a name connects through a us_connecting_socket_t");
    h.run_until(received_echo);
    // The second connect finds the result in the cache, and still races ::1 and 127.0.0.1.
    EVENTS.with(|e| e.borrow_mut().clear());
    assert_eq!(connect(&h, c"localhost", port), 0);
    h.run_until(received_echo);
    h.close_all();
}

#[test]
fn a_refused_connect_reports_econnrefused() {
    let h = Harness::new();
    let (ls, port) = listen_tcp(&h);
    // SAFETY: the live listener; its port is free again after this tick.
    unsafe { us_listen_socket_close(ls) };
    h.run_until(|_| true);
    connect(&h, c"127.0.0.1", port);
    h.run_until(|e| e.contains(&Event::ConnectError(libc::ECONNREFUSED)));
    h.close_all();
}

#[test]
fn unix_listen_accept_and_echo() {
    let h = Harness::new();
    let path = std::env::temp_dir().join(format!("usockets-rs-{}.sock", std::process::id()));
    let _ = std::fs::remove_file(&path);
    let path = CString::new(path.to_str().unwrap()).unwrap();
    let mut error = 0;
    // SAFETY: the harness's group and a path.
    unsafe {
        let ls = us_socket_group_listen_unix(h.server, SERVER, ptr::null_mut(), path.as_ptr(), path.as_bytes().len(), 0, 0, &mut error);
        assert!(!ls.is_null(), "listen: errno {error}");
        let s = us_socket_group_connect_unix(h.client, CLIENT, ptr::null_mut(), path.as_ptr(), path.as_bytes().len(), 0, 0);
        assert!(!s.is_null());
    }
    h.run_until(received_echo);
    h.close_all();
    let _ = std::fs::remove_file(path.to_str().unwrap());
}

#[test]
fn adoption_moves_a_socket_to_a_bigger_ext() {
    let h = Harness::new();
    let into = new_group(h.loop_);
    ADOPT_INTO.with(|a| *a.borrow_mut() = Some(into as usize));
    let (_ls, port) = listen_tcp(&h);
    connect(&h, c"127.0.0.1", port);
    h.run_until(received_echo);
    ADOPT_INTO.with(|a| *a.borrow_mut() = None);
    let events = events();
    assert!(events.contains(&Event::Data(ADOPTED, b"hello".to_vec())), "{events:?}");
    assert!(!events.contains(&Event::Data(SERVER, b"hello".to_vec())), "{events:?}");
    assert!(ADOPTED_EXT.with(|e| e.borrow().is_some()));
    // SAFETY: the groups of this loop.
    unsafe { us_socket_group_close_all(into) };
    h.close_all();
}

#[test]
fn ipc_passes_an_fd_with_its_message() {
    let h = Harness::new();
    let mut fds = [0 as c_int; 2];
    let mut pipe = [0 as c_int; 2];
    // SAFETY: room for the fds.
    unsafe {
        assert_eq!(libc::socketpair(libc::AF_UNIX, libc::SOCK_STREAM, 0, fds.as_mut_ptr()), 0);
        assert_eq!(libc::pipe(pipe.as_mut_ptr()), 0);
        let reader = us_socket_from_fd(h.server, IPC, ptr::null_mut(), 0, fds[0], 0, 1);
        let writer = us_socket_from_fd(h.client, IPC, ptr::null_mut(), 0, fds[1], 0, 1);
        assert!(!reader.is_null() && !writer.is_null());
        assert_eq!(us_socket_ipc_write_fd(writer, c"x".as_ptr(), 1, pipe[0]), 1);
        libc::close(pipe[0]);
        libc::close(pipe[1]);
    }
    h.run_until(|e| e.contains(&Event::Data(IPC, b"x".to_vec())));
    let events = events();
    let fd_at = events.iter().position(|e| *e == Event::Fd(IPC)).expect("the fd arrives");
    let data_at = events.iter().position(|e| *e == Event::Data(IPC, b"x".to_vec())).unwrap();
    assert!(fd_at < data_at, "on_fd comes before the message it came with: {events:?}");
    h.close_all();
}

// ── TLS ────────────────────────────────────────────────────────────────────

#[derive(Default)]
struct Tls<'a> {
    cert: Option<&'a str>,
    key: Option<&'a str>,
    ca: Option<&'a str>,
    passphrase: Option<&'a str>,
    request_cert: bool,
    reject_unauthorized: bool,
}

fn fixture(name: &str) -> CString {
    let path = format!("{}/tests/fixtures/tls/{name}", env!("CARGO_MANIFEST_DIR"));
    CString::new(std::fs::read(path).unwrap()).unwrap()
}

/// An SSL context from these options, through `us_ssl_ctx_from_options` as `bun_uws_sys` calls it; null if it
/// failed.
fn tls_ctx(tls: Tls<'_>) -> *mut c_void {
    let [cert, key, ca] = [tls.cert, tls.key, tls.ca].map(|name| name.map(fixture));
    let [cert, key, ca] = [&cert, &key, &ca].map(|pem| pem.as_ref().map(|pem| pem.as_ptr()));
    let passphrase = tls.passphrase.map(|p| CString::new(p).unwrap());
    let mut options = bun_uws_sys::BunSocketContextOptions::default();
    if let Some(cert) = &cert {
        options.cert = cert;
        options.cert_count = 1;
    }
    if let Some(key) = &key {
        options.key = key;
        options.key_count = 1;
    }
    if let Some(ca) = &ca {
        options.ca = ca;
        options.ca_count = 1;
    }
    options.passphrase = passphrase.as_ref().map_or(ptr::null(), |p| p.as_ptr());
    options.request_cert = c_int::from(tls.request_cert);
    options.reject_unauthorized = c_int::from(tls.reject_unauthorized);
    let mut error = 0;
    // SAFETY: options whose strings outlive the call.
    unsafe { us_ssl_ctx_from_options(options, &mut error) }
}

fn server_ctx() -> *mut c_void {
    let ctx = tls_ctx(Tls { cert: Some("server.crt"), key: Some("server-key.txt"), ..Tls::default() });
    assert!(!ctx.is_null());
    ctx
}

fn client_ctx() -> *mut c_void {
    let ctx = tls_ctx(Tls { ca: Some("ca.crt"), ..Tls::default() });
    assert!(!ctx.is_null());
    ctx
}

fn listen_tls(h: &Harness, ctx: *mut c_void) -> (*mut c_void, c_int) {
    let mut error = 0;
    // SAFETY: the harness's group and a live context.
    let ls = unsafe { us_socket_group_listen(h.server, TLS_SERVER, ctx, c"127.0.0.1".as_ptr(), 0, 0, 0, &mut error) };
    assert!(!ls.is_null(), "listen: errno {error}");
    // SAFETY: a live listener.
    (ls, unsafe { us_listen_socket_port(ls) })
}

fn connect_tls(h: &Harness, ctx: *mut c_void, port: c_int) {
    let mut resolved = 0;
    // SAFETY: the harness's group and a live context.
    let s = unsafe {
        us_socket_group_connect(h.client, TLS_CLIENT, ctx, c"127.0.0.1".as_ptr(), port, ptr::null(), 0, 0, 0, &mut resolved)
    };
    assert!(!s.is_null());
}

fn received_tls_echo(events: &[Event]) -> bool {
    events.contains(&Event::Data(TLS_CLIENT, b"echo:hello".to_vec())) && events.contains(&Event::Close(TLS_CLIENT))
}

fn handshakes(kind: u8) -> Vec<Event> {
    events().into_iter().filter(|e| matches!(e, Event::Handshake(k, ..) if *k == kind)).collect()
}

fn free_ctxs(ctxs: &[*mut c_void]) {
    for &ctx in ctxs {
        // SAFETY: the test's refs.
        unsafe { SSL_CTX_free(ctx) };
    }
}

#[test]
fn tls_server_and_client_echo() {
    let h = Harness::new();
    SNI.with(|n| *n.borrow_mut() = None);
    let server = tls_ctx(Tls {
        cert: Some("server.crt"),
        key: Some("server-key-encrypted.txt"),
        passphrase: Some("hunter2"),
        ..Tls::default()
    });
    assert!(!server.is_null(), "the key opens with its passphrase");
    let wrong = Tls { cert: Some("server.crt"), key: Some("server-key-encrypted.txt"), passphrase: Some("nope"), ..Tls::default() };
    assert!(tls_ctx(wrong).is_null());
    let client = client_ctx();
    let (_ls, port) = listen_tls(&h, server);
    connect_tls(&h, client, port);
    h.run_until(received_tls_echo);
    assert!(events().contains(&Event::Data(TLS_SERVER, b"hello".to_vec())), "{:?}", events());
    assert_eq!(handshakes(TLS_CLIENT), [Event::Handshake(TLS_CLIENT, true, 0, false)]);
    // A server that asks for no certificate gets none: X509_V_ERR_UNABLE_TO_GET_ISSUER_CERT, as BoringSSL's.
    assert_eq!(handshakes(TLS_SERVER), [Event::Handshake(TLS_SERVER, true, 2, false)]);
    h.run_until(|e| e.contains(&Event::End(TLS_SERVER)) && e.contains(&Event::Close(TLS_SERVER)));

    // Without the CA the handshake still finishes, and the owner reads why the chain does not verify.
    EVENTS.with(|e| e.borrow_mut().clear());
    let untrusting = tls_ctx(Tls::default());
    connect_tls(&h, untrusting, port);
    h.run_until(received_tls_echo);
    assert_eq!(handshakes(TLS_CLIENT), [Event::Handshake(TLS_CLIENT, true, 20, false)]);
    h.close_all();
    free_ctxs(&[server, client, untrusting]);
}

#[test]
fn client_certificates_reach_the_server() {
    let h = Harness::new();
    SNI.with(|n| *n.borrow_mut() = None);
    let server = tls_ctx(Tls {
        cert: Some("server.crt"),
        key: Some("server-key.txt"),
        ca: Some("ca.crt"),
        request_cert: true,
        reject_unauthorized: true,
        ..Tls::default()
    });
    let client = tls_ctx(Tls { cert: Some("client.crt"), key: Some("client-key.txt"), ca: Some("ca.crt"), ..Tls::default() });
    assert!(!server.is_null() && !client.is_null());
    let (_ls, port) = listen_tls(&h, server);
    connect_tls(&h, client, port);
    h.run_until(received_tls_echo);
    assert_eq!(handshakes(TLS_SERVER), [Event::Handshake(TLS_SERVER, true, 0, false)]);

    // reject_unauthorized with request_cert: a client without a certificate fails the server's handshake.
    EVENTS.with(|e| e.borrow_mut().clear());
    let anonymous = client_ctx();
    connect_tls(&h, anonymous, port);
    h.run_until(|e| e.iter().any(|e| matches!(e, Event::Handshake(TLS_SERVER, false, ..))) && e.contains(&Event::Close(TLS_CLIENT)));
    assert!(!events().contains(&Event::Data(TLS_SERVER, b"hello".to_vec())), "{:?}", events());
    h.close_all();
    free_ctxs(&[server, client, anonymous]);
}

#[test]
fn sessions_resume_through_the_context() {
    let h = Harness::new();
    SNI.with(|n| *n.borrow_mut() = None);
    let (server, client) = (server_ctx(), client_ctx());
    let (_ls, port) = listen_tls(&h, server);
    connect_tls(&h, client, port);
    h.run_until(received_tls_echo);
    EVENTS.with(|e| e.borrow_mut().clear());
    connect_tls(&h, client, port);
    h.run_until(received_tls_echo);
    assert_eq!(handshakes(TLS_CLIENT), [Event::Handshake(TLS_CLIENT, true, 0, true)]);
    assert!(handshakes(TLS_SERVER).iter().all(|e| matches!(e, Event::Handshake(_, true, _, true))), "{:?}", events());
    h.close_all();
    free_ctxs(&[server, client]);
}

#[test]
fn sni_picks_the_context_of_the_name() {
    let h = Harness::new();
    let server = server_ctx();
    let named = tls_ctx(Tls { cert: Some("sni.crt"), key: Some("sni-key.txt"), ..Tls::default() });
    assert!(!named.is_null());
    let client = client_ctx();
    let (ls, port) = listen_tls(&h, server);
    // SAFETY: a live TLS listener and context.
    unsafe { us_listen_socket_add_server_name(ls, c"*.sni.test".as_ptr(), named) };

    SNI.with(|n| *n.borrow_mut() = Some(c"a.sni.test".to_owned()));
    connect_tls(&h, client, port);
    h.run_until(received_tls_echo);
    assert_eq!(handshakes(TLS_CLIENT), [Event::Handshake(TLS_CLIENT, true, 0, false)], "*.sni.test's certificate");
    assert_eq!(SERVER_SAW.with(|s| s.borrow().clone()), Some((Some("a.sni.test".to_owned()), named as usize)));

    // A name that no pattern has gets the listener's context, whose certificate is not for it.
    EVENTS.with(|e| e.borrow_mut().clear());
    SNI.with(|n| *n.borrow_mut() = Some(c"other.test".to_owned()));
    connect_tls(&h, client, port);
    h.run_until(received_tls_echo);
    assert_eq!(handshakes(TLS_CLIENT), [Event::Handshake(TLS_CLIENT, true, 62, false)]);
    assert_eq!(SERVER_SAW.with(|s| s.borrow().clone()), Some((Some("other.test".to_owned()), server as usize)));
    SNI.with(|n| *n.borrow_mut() = None);
    h.close_all();
    free_ctxs(&[server, named, client]);
}
