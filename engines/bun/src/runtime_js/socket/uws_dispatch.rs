//! Socket event dispatch (src/runtime/socket/uws_dispatch.rs): uSockets (`bun_usockets_rs`) calls these
//! `us_dispatch_*` for every socket of a loop, and each goes to the vtable of its kind. A `Dynamic` socket carries
//! its vtable on its group; the kinds whose handlers are in a crate this one does not link (the HTTP client
//! thread's, which `bun install` drives) come from the table [`register_kinds`] was given.

use core::ffi::{c_int, c_void};
use std::sync::OnceLock;

use bun_uws_sys::socket_group::VTable;
use bun_uws_sys::{ConnectingSocket, SocketKind, us_bun_verify_error_t, us_socket_t};

static KINDS: OnceLock<fn(SocketKind) -> Option<&'static VTable>> = OnceLock::new();

/// The vtables of the kinds that another crate handles. Once, before a socket of theirs exists.
pub fn register_kinds(kinds: fn(SocketKind) -> Option<&'static VTable>) {
    let _ = KINDS.set(kinds);
}

fn table(kind: SocketKind, group_vtable: impl FnOnce() -> Option<&'static VTable>) -> &'static VTable {
    match kind {
        SocketKind::Dynamic => group_vtable().expect("a dynamic socket's group has a vtable"),
        other => KINDS
            .get()
            .and_then(|kinds| kinds(other))
            .unwrap_or_else(|| panic!("a {other:?} socket: nothing in this build handles its kind")),
    }
}

fn vt(s: *mut us_socket_t) -> &'static VTable {
    let s = us_socket_t::opaque_mut(s);
    let kind = s.kind();
    table(kind, || s.raw_group().vtable.get())
}

fn vtc(c: *mut ConnectingSocket) -> &'static VTable {
    let c = ConnectingSocket::opaque_mut(c);
    let kind = c.kind();
    // SAFETY: a connecting socket with a kind has a group.
    table(kind, || unsafe { (*c.raw_group()).vtable.get() })
}

/// The SSL context of a TLS server socket's ClientHello name (src/runtime/socket/Listener.rs), for a socket adopted
/// into TLS without a listen socket. Nothing here makes one with an SNICallback yet: no context, so the listener's
/// is used.
#[unsafe(no_mangle)]
extern "C" fn us_dispatch_socket_server_name(
    _s: *mut us_socket_t,
    _hostname: *const core::ffi::c_char,
    _abort: *mut c_int,
) -> *mut c_void {
    core::ptr::null_mut()
}

macro_rules! us_dispatch_shims {
    ($(
        fn $name:ident($recv:ident: *mut $Recv:ty $(, $a:ident: $t:ty)* $(,)?) -> $ret:ty
            = $lookup:ident.$field:ident($($call:expr),* $(,)?) or $default:expr;
    )*) => {$(
        /// # Safety
        /// uSockets passes a live socket, and buffers that are valid for the call.
        #[unsafe(no_mangle)]
        #[allow(clippy::unused_unit)]
        unsafe extern "C" fn $name($recv: *mut $Recv $(, $a: $t)*) -> $ret {
            match $lookup($recv).$field {
                // SAFETY: the vtable's callback for this kind, whose signature this shim has.
                Some(f) => unsafe { f($($call),*) },
                None => $default,
            }
        }
    )*};
}

us_dispatch_shims! {
    fn us_dispatch_open(s: *mut us_socket_t, is_client: c_int, ip: *mut u8, ip_len: c_int) -> *mut us_socket_t
        = vt.on_open(s, is_client, ip, ip_len) or s;
    fn us_dispatch_data(s: *mut us_socket_t, data: *mut u8, len: c_int) -> *mut us_socket_t
        = vt.on_data(s, data, len) or s;
    fn us_dispatch_fd(s: *mut us_socket_t, fd: c_int) -> *mut us_socket_t
        = vt.on_fd(s, fd) or s;
    fn us_dispatch_writable(s: *mut us_socket_t) -> *mut us_socket_t
        = vt.on_writable(s) or s;
    fn us_dispatch_close(s: *mut us_socket_t, code: c_int, reason: *mut c_void) -> *mut us_socket_t
        = vt.on_close(s, code, reason) or s;
    fn us_dispatch_timeout(s: *mut us_socket_t) -> *mut us_socket_t
        = vt.on_timeout(s) or s;
    fn us_dispatch_long_timeout(s: *mut us_socket_t) -> *mut us_socket_t
        = vt.on_long_timeout(s) or s;
    fn us_dispatch_end(s: *mut us_socket_t) -> *mut us_socket_t
        = vt.on_end(s) or s;
    fn us_dispatch_connect_error(s: *mut us_socket_t, code: c_int) -> *mut us_socket_t
        = vt.on_connect_error(s, code) or s;
    fn us_dispatch_connecting_error(c: *mut ConnectingSocket, code: c_int) -> *mut ConnectingSocket
        = vtc.on_connecting_error(c, code) or c;
    fn us_dispatch_handshake(s: *mut us_socket_t, ok: c_int, err: us_bun_verify_error_t) -> ()
        = vt.on_handshake(s, ok, err, core::ptr::null_mut()) or ();
}
