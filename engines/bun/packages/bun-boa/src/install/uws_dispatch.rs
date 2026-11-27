//! The HTTP client thread's sockets (src/runtime/socket/uws_handlers.rs's `HTTPClient`), whose kinds
//! `bun_runtime_js::socket::uws_dispatch` sends here.

use core::ffi::c_void;

use bun_uws_sys::socket::NewSocketHandler;
use bun_uws_sys::socket_group::VTable;
use bun_uws_sys::vtable::{self, Handler as VHandler};
use bun_uws_sys::{ConnectingSocket, SocketKind, us_bun_verify_error_t, us_socket_t};

type HttpH<const SSL: bool> = bun_http::http_context::Handler<SSL>;

/// src/runtime/socket/uws_handlers.rs's `HTTPClient`.
struct HTTPClient<const SSL: bool>;

impl<const SSL: bool> VHandler for HTTPClient<SSL> {
    type Ext = ();
    const HAS_EXT: bool = false;

    const HAS_ON_OPEN: bool = true;
    const HAS_ON_DATA: bool = true;
    const HAS_ON_WRITABLE: bool = true;
    const HAS_ON_CLOSE: bool = true;
    const HAS_ON_TIMEOUT: bool = true;
    const HAS_ON_LONG_TIMEOUT: bool = true;
    const HAS_ON_END: bool = true;
    const HAS_ON_CONNECT_ERROR: bool = true;
    const HAS_ON_CONNECTING_ERROR: bool = true;
    const HAS_ON_HANDSHAKE: bool = true;

    fn on_open_no_ext(s: *mut us_socket_t, _is_client: bool, _ip: &[u8]) {
        HttpH::<SSL>::on_open(NewSocketHandler::<SSL>::from(s));
    }
    fn on_data_no_ext(s: *mut us_socket_t, data: &[u8]) {
        HttpH::<SSL>::on_data(NewSocketHandler::<SSL>::from(s), data);
    }
    fn on_writable_no_ext(s: *mut us_socket_t) {
        HttpH::<SSL>::on_writable(NewSocketHandler::<SSL>::from(s));
    }
    fn on_close_no_ext(s: *mut us_socket_t, code: i32, reason: Option<*mut c_void>) {
        HttpH::<SSL>::on_close(NewSocketHandler::<SSL>::from(s), code, reason);
    }
    fn on_timeout_no_ext(s: *mut us_socket_t) {
        HttpH::<SSL>::on_timeout(NewSocketHandler::<SSL>::from(s));
    }
    fn on_long_timeout_no_ext(s: *mut us_socket_t) {
        HttpH::<SSL>::on_long_timeout(NewSocketHandler::<SSL>::from(s));
    }
    fn on_end_no_ext(s: *mut us_socket_t) {
        HttpH::<SSL>::on_end(NewSocketHandler::<SSL>::from(s));
    }
    fn on_connect_error_no_ext(s: *mut us_socket_t, code: i32) {
        HttpH::<SSL>::on_connect_error(NewSocketHandler::<SSL>::from(s), code);
    }
    fn on_connecting_error(cs: *mut ConnectingSocket, code: i32) {
        HttpH::<SSL>::on_connect_error(NewSocketHandler::<SSL>::from_connecting(cs), code);
    }
    fn on_handshake_no_ext(s: *mut us_socket_t, ok: bool, err: us_bun_verify_error_t) {
        HttpH::<SSL>::on_handshake(NewSocketHandler::<SSL>::from(s), ok as i32, err);
    }
}

fn kinds(kind: SocketKind) -> Option<&'static VTable> {
    match kind {
        SocketKind::HttpClient => Some(vtable::make::<HTTPClient<false>>()),
        SocketKind::HttpClientTls => Some(vtable::make::<HTTPClient<true>>()),
        _ => None,
    }
}

/// Sends the HTTP client's sockets here; before the package manager starts its thread.
pub fn register() {
    bun_runtime_js::socket::uws_dispatch::register_kinds(kinds);
}
