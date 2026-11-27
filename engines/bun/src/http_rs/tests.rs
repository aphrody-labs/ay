use std::net::SocketAddr;
use std::rc::Rc;

use crate::client::{self, Redirect, Request};
use crate::decompress::{self, Encoding};
use crate::reactor::Reactor;
use crate::server::{Bind, IncomingRequest, Responder, ResponseHead, Server, Service, http_date};
use crate::Url;

// bun_alloc's out-of-memory hook, which bun_crash_handler defines in a binary.
#[unsafe(no_mangle)]
extern "Rust" fn __bun_crash_handler_out_of_memory() -> ! {
    panic!("out of memory");
}

/// Answers `/redirect/<n>` with a redirect to `/redirect/<n-1>`, `/gzip` with a gzip body, anything else with
/// `<method> <target> <body>`.
struct Echo;

impl Service for Echo {
    fn request(&self, responder: Responder, request: IncomingRequest) {
        if let Some(n) = request.target.strip_prefix("/redirect/") {
            let n: u32 = n.parse().unwrap();
            let location = if n == 0 { "/done".to_owned() } else { format!("/redirect/{}", n - 1) };
            let headers = [(b"Location".to_vec(), location.into_bytes())];
            return responder.respond(ResponseHead { status: 302, status_text: "", headers: &headers }, b"");
        }
        if request.target == "/gzip" {
            let body =
                bun_deflate::compress_to_vec(b"hello gzip", bun_deflate::Format::Gzip, bun_deflate::Level::DEFAULT)
                    .unwrap();
            let headers = [(b"Content-Encoding".to_vec(), b"gzip".to_vec())];
            return responder.respond(ResponseHead { status: 200, status_text: "", headers: &headers }, &body);
        }
        let body = format!("{} {} {}", request.method, request.target, String::from_utf8_lossy(&request.body));
        let headers = [(b"X-Agent".to_vec(), request.header(b"user-agent").unwrap_or_default().to_vec())];
        responder.respond(ResponseHead { status: 201, status_text: "", headers: &headers }, body.as_bytes());
    }
}

fn serve(reactor: &Rc<Reactor>) -> Rc<Server> {
    let addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
    Server::listen(reactor, Bind::Tcp(addr), None, Rc::new(Echo)).unwrap()
}

fn url(server: &Server, path: &str) -> Url {
    Url::parse(&format!("http://{}{path}", server.local_addr().unwrap())).unwrap()
}

#[test]
fn request_and_response() {
    let reactor = Reactor::new().unwrap();
    let server = serve(&reactor);
    let mut request = Request::get(url(&server, "/a?b=1"));
    request.method = "POST".into();
    request.body = b"payload".to_vec();
    request.user_agent = "Bun/test".into();
    let (head, body) = reactor.block_on(client::fetch(&reactor, request)).unwrap();
    assert_eq!(head.status, 201);
    assert_eq!(head.status_text, b"Created");
    assert_eq!(head.header(b"x-agent"), Some(&b"Bun/test"[..]));
    assert_eq!(body, b"POST /a?b=1 payload");
    server.stop(true);
    assert!(!reactor.is_alive());
}

#[test]
fn follows_redirects() {
    let reactor = Reactor::new().unwrap();
    let server = serve(&reactor);
    let (head, body) = reactor.block_on(client::fetch(&reactor, Request::get(url(&server, "/redirect/3")))).unwrap();
    assert!(head.redirected);
    assert_eq!(head.url.path(), "/done");
    assert_eq!(body, b"GET /done ");

    let mut manual = Request::get(url(&server, "/redirect/0"));
    manual.redirect = Redirect::Manual;
    let (head, _) = reactor.block_on(client::fetch(&reactor, manual)).unwrap();
    assert_eq!(head.status, 302);

    let error = reactor.block_on(client::fetch(&reactor, Request::get(url(&server, "/redirect/30")))).unwrap_err();
    assert_eq!(error.code(), "TooManyRedirects");
    server.stop(true);
}

#[test]
fn decodes_gzip() {
    let reactor = Reactor::new().unwrap();
    let server = serve(&reactor);
    let (_, body) = reactor.block_on(client::fetch(&reactor, Request::get(url(&server, "/gzip")))).unwrap();
    assert_eq!(body, b"hello gzip");
    server.stop(true);
}

#[test]
fn connection_refused() {
    let reactor = Reactor::new().unwrap();
    // A port that was just free.
    let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    let request = Request::get(Url::parse(&format!("http://127.0.0.1:{port}/")).unwrap());
    let error = reactor.block_on(client::fetch(&reactor, request)).unwrap_err();
    assert_eq!(error.code(), "ECONNREFUSED");
    assert!(error.message("").contains("Unable to connect"));
}

#[test]
fn serializes_requests_as_bun() {
    let mut request = Request::get(Url::parse("http://user:p%40ss@example.com:8080/x?y").unwrap());
    request.user_agent = "Bun/1".into();
    request.headers.push(("X-Custom".into(), b"1".to_vec()));
    let text = String::from_utf8(client::serialize(&request)).unwrap();
    assert_eq!(
        text,
        "GET /x?y HTTP/1.1\r\nX-Custom: 1\r\nConnection: keep-alive\r\nUser-Agent: Bun/1\r\nAccept: */*\r\nHost: example.com:8080\r\nAccept-Encoding: gzip, deflate, br, zstd\r\nAuthorization: Basic dXNlcjpwQHNz\r\n\r\n"
    );
}

#[test]
fn content_codings() {
    let raw = bun_deflate::compress_to_vec(b"abc", bun_deflate::Format::Raw, bun_deflate::Level::DEFAULT).unwrap();
    assert_eq!(decompress::decode(Encoding::Deflate, &raw).unwrap(), b"abc");
    let zlib = bun_deflate::compress_to_vec(b"abc", bun_deflate::Format::Zlib, bun_deflate::Level::DEFAULT).unwrap();
    assert_eq!(decompress::decode(Encoding::Deflate, &zlib).unwrap(), b"abc");
    assert_eq!(Encoding::parse(b" GZIP "), Some(Encoding::Gzip));
}

#[test]
fn dates() {
    let time = std::time::UNIX_EPOCH + std::time::Duration::from_secs(784_111_777);
    assert_eq!(http_date(time), "Sun, 06 Nov 1994 08:49:37 GMT");
}
