//! Groups and connects: context.cpp, the connecting-socket half of socket.c, and bsd.cpp's connect.

use core::ffi::{CStr, c_char, c_int, c_uint, c_void};
use core::ptr;
use std::net::SocketAddr;

use crate::bsd::{connect as bsd_connect, set_nodelay, sockaddr_from_std};
use crate::eventing::*;
use crate::socket::*;
use crate::*;

const CONCURRENT_CONNECTIONS: usize = 4;

// ── groups ─────────────────────────────────────────────────────────────────

#[unsafe(no_mangle)]
pub unsafe extern "C" fn us_socket_group_init(
    group: *mut us_socket_group_t,
    loop_: *mut us_loop_t,
    vtable: *const c_void,
    ext: *mut c_void,
) {
    // SAFETY: the caller passes room for a group, which all-zero is a valid one of.
    unsafe {
        ptr::write_bytes(group, 0, 1);
        (*group).loop_ = loop_;
        (*group).vtable = vtable;
        (*group).ext = ext;
    }
}

/// The owner frees the group next: it must be empty (close_all first) and not be dispatching.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn us_socket_group_deinit(group: *mut us_socket_group_t) {
    // SAFETY: the caller passes an initialized group.
    unsafe {
        debug_assert!((*group).head_sockets.is_null() && (*group).head_connecting_sockets.is_null());
        if (*group).linked != 0 {
            loop_unlink_group((*group).loop_, group);
            (*group).linked = 0;
        }
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn us_socket_group_close_all(group: *mut us_socket_group_t) {
    // SAFETY: the caller's group.
    unsafe { group_close_all(group, true) }
}

/// Listeners first when `also_listeners`, so that nothing new is accepted while the sockets close. Process exit
/// leaves listeners to their owners, which close them themselves.
pub(crate) unsafe fn group_close_all(group: *mut us_socket_group_t, also_listeners: bool) {
    // SAFETY: the caller passes an initialized group. Close handlers can close siblings, which `iterator` follows.
    unsafe {
        let loop_ = (*group).loop_;
        if loop_.is_null() {
            return;
        }
        (*loop_).data.tick_depth += 1;
        if also_listeners {
            while !(*group).head_listen_sockets.is_null() {
                crate::listen::us_listen_socket_close((*group).head_listen_sockets);
            }
        }
        let mut c = (*group).head_connecting_sockets;
        while !c.is_null() {
            let next = (*c).next_pending;
            us_connecting_socket_close(c);
            c = next;
        }
        (*group).iterator = (*group).head_sockets;
        while !(*group).iterator.is_null() {
            let s = (*group).iterator;
            (*group).iterator = (*s).next;
            us_socket_close(s, LIBUS_SOCKET_CLOSE_CODE_CLEAN_SHUTDOWN, ptr::null_mut());
        }
        (*group).iterator = ptr::null_mut();
        (*loop_).data.tick_depth -= 1;
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn us_socket_group_loop(group: *mut us_socket_group_t) -> *mut us_loop_t {
    // SAFETY: the caller passes an initialized group.
    unsafe { (*group).loop_ }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn us_socket_group_ext(group: *mut us_socket_group_t) -> *mut c_void {
    // SAFETY: as above.
    unsafe { (*group).ext }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn us_socket_group_timestamp(group: *mut us_socket_group_t) -> u16 {
    // SAFETY: as above.
    unsafe { u16::from((*group).timestamp) }
}

pub(crate) unsafe fn group_touched(group: *mut us_socket_group_t) {
    // SAFETY: the caller passes an initialized group.
    unsafe {
        if (*group).linked == 0 {
            loop_link_group((*group).loop_, group);
            (*group).linked = 1;
        }
    }
}

pub(crate) unsafe fn group_maybe_unlink(group: *mut us_socket_group_t) {
    // SAFETY: as above.
    unsafe {
        if (*group).linked != 0
            && (*group).head_sockets.is_null()
            && (*group).head_connecting_sockets.is_null()
            && (*group).head_listen_sockets.is_null()
            && (*group).low_prio_count == 0
        {
            loop_unlink_group((*group).loop_, group);
            (*group).linked = 0;
        }
    }
}

unsafe fn group_link_connecting(group: *mut us_socket_group_t, c: *mut us_connecting_socket_t) {
    // SAFETY: the caller passes an initialized group and a live connecting socket.
    unsafe {
        if (*c).closed {
            return;
        }
        (*c).group = group;
        (*c).next_pending = (*group).head_connecting_sockets;
        (*c).prev_pending = ptr::null_mut();
        if !(*group).head_connecting_sockets.is_null() {
            (*(*group).head_connecting_sockets).prev_pending = c;
        }
        (*group).head_connecting_sockets = c;
        group_touched(group);
        us_internal_enable_sweep_timer((*group).loop_);
    }
}

unsafe fn group_unlink_connecting(group: *mut us_socket_group_t, c: *mut us_connecting_socket_t) {
    // SAFETY: the caller passes a group and one of its connecting sockets.
    unsafe {
        let (prev, next) = ((*c).prev_pending, (*c).next_pending);
        if prev == next {
            (*group).head_connecting_sockets = ptr::null_mut();
        } else {
            if prev.is_null() {
                (*group).head_connecting_sockets = next;
            } else {
                (*prev).next_pending = next;
            }
            if !next.is_null() {
                (*next).prev_pending = prev;
            }
        }
        us_internal_disable_sweep_timer((*group).loop_);
        group_maybe_unlink(group);
    }
}

/// An adopted socket's pending connect follows it to `group`.
pub(crate) unsafe fn group_move_connecting(c: *mut us_connecting_socket_t, group: *mut us_socket_group_t) {
    // SAFETY: the caller's live connecting socket and initialized group.
    unsafe {
        if !(*c).group.is_null() {
            group_unlink_connecting((*c).group, c);
        }
        group_link_connecting(group, c);
    }
}

// ── connect ────────────────────────────────────────────────────────────────

unsafe fn new_connect_socket(group: *mut us_socket_group_t, fd: c_int, kind: u8, options: c_int, ext_size: c_int) -> *mut us_socket_t {
    // SAFETY: the caller passes an initialized group and a connecting fd it hands over.
    unsafe {
        let loop_ = (*group).loop_;
        let size = core::mem::size_of::<us_socket_t>() + ext_size.max(0) as usize;
        let s = us_create_poll(loop_, false, size).cast::<us_socket_t>();
        (*s).alloc_size = size;
        (*s).p.fd = fd;
        (*s).p.poll_type = POLL_TYPE_SEMI_SOCKET;
        if us_poll_start_rc(&mut (*s).p, loop_, LIBUS_SOCKET_WRITABLE) != 0 {
            libc::close(fd);
            us_poll_free(s.cast(), size, loop_);
            return ptr::null_mut();
        }
        (*s).group = group;
        (*s).kind = kind;
        (*s).timeout = 255;
        (*s).long_timeout = 255;
        (*s).allow_half_open = options & LIBUS_SOCKET_ALLOW_HALF_OPEN != 0;
        group_link_socket(group, s);
        s
    }
}

unsafe fn connect_resolved(
    group: *mut us_socket_group_t,
    kind: u8,
    address: &SocketAddr,
    local: Option<&SocketAddr>,
    options: c_int,
    ext_size: c_int,
) -> *mut us_socket_t {
    let (storage, len) = sockaddr_from_std(address);
    let family = if address.is_ipv4() { libc::AF_INET } else { libc::AF_INET6 };
    let fd = bsd_connect(family, (&raw const storage).cast(), len, local);
    if fd == -1 {
        return ptr::null_mut();
    }
    set_nodelay(fd, 1);
    // SAFETY: the caller's group; `fd` is ours to hand over.
    unsafe { new_connect_socket(group, fd, kind, options, ext_size) }
}

/// The SSL of a client socket, on its context, named for `host` until on_open sets SNI.
unsafe fn with_tls(s: *mut us_socket_t, ssl_ctx: *mut c_void, host: Option<&CStr>) -> *mut us_socket_t {
    if !s.is_null() && !ssl_ctx.is_null() {
        // SAFETY: a socket just made, and the caller's context.
        unsafe { crate::tls::attach_client(s, ssl_ctx.cast(), host) };
    }
    s
}

/// A `us_socket_t` (`*has_dns_resolved = 1`) for an address, else a `us_connecting_socket_t` that resolves the name;
/// null if the connect failed at once.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn us_socket_group_connect(
    group: *mut us_socket_group_t,
    kind: u8,
    ssl_ctx: *mut c_void,
    host: *const c_char,
    port: c_int,
    local_host: *const c_char,
    local_port: c_int,
    options: c_int,
    socket_ext_size: c_int,
    has_dns_resolved: *mut c_int,
) -> *mut c_void {
    // SAFETY: the caller passes an initialized group, NUL-terminated strings (or a null local host) and an out-int.
    unsafe {
        let port = port as u16;
        let local = (!local_host.is_null())
            .then(|| crate::dns::parse_ip(CStr::from_ptr(local_host), local_port as u16))
            .flatten();
        let host = CStr::from_ptr(host);
        if let Some(address) = crate::dns::parse_ip(host, port) {
            *has_dns_resolved = 1;
            let s = connect_resolved(group, kind, &address, local.as_ref(), options, socket_ext_size);
            return with_tls(s, ssl_ctx, Some(host)).cast();
        }
        let mut req: *const crate::dns::Request = ptr::null();
        if crate::dns::Bun__addrinfo_get((*group).loop_, host.as_ptr(), port, &mut req) == 0 {
            // A cached failure goes the connecting-socket way below, to be reported as an uncached one is.
            let result = crate::dns::Bun__addrinfo_getRequestResult(req);
            if result.error == 0 && !result.entries.is_null() && (*result.entries.cast::<libc::addrinfo>()).ai_next.is_null() {
                let info = &*result.entries.cast::<libc::addrinfo>();
                let address = crate::dns::sockaddr_to_std(info.ai_addr).map(|mut a| {
                    a.set_port(port);
                    a
                });
                *has_dns_resolved = 1;
                let s = address.map_or(ptr::null_mut(), |a| connect_resolved(group, kind, &a, local.as_ref(), options, socket_ext_size));
                crate::dns::Bun__addrinfo_freeRequest(req, c_int::from(s.is_null()));
                return with_tls(s, ssl_ctx, Some(host)).cast();
            }
        }
        *has_dns_resolved = 0;
        let loop_ = (*group).loop_;
        let size = core::mem::size_of::<us_connecting_socket_t>() + socket_ext_size.max(0) as usize;
        let c = xcalloc(size).cast::<us_connecting_socket_t>();
        (*c).alloc_size = size;
        (*c).socket_ext_size = socket_ext_size;
        (*c).options = options;
        (*c).kind = kind;
        (*c).loop_ = loop_;
        (*c).timeout = 255;
        (*c).long_timeout = 255;
        (*c).pending_resolve_callback = true;
        (*c).port = port;
        if !ssl_ctx.is_null() {
            crate::tls::SSL_CTX_up_ref(ssl_ctx.cast());
            (*c).ssl_ctx = ssl_ctx.cast();
        }
        (*c).addrinfo_req = req.cast();
        group_link_connecting(group, c);
        (*loop_).num_polls += 1;
        crate::dns::Bun__addrinfo_set(req, c);
        c.cast()
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn us_socket_group_connect_unix(
    group: *mut us_socket_group_t,
    kind: u8,
    ssl_ctx: *mut c_void,
    path: *const c_char,
    pathlen: usize,
    options: c_int,
    socket_ext_size: c_int,
) -> *mut us_socket_t {
    // SAFETY: the caller passes an initialized group and `pathlen` bytes at `path`.
    unsafe {
        let path = if pathlen == 0 { &[][..] } else { core::slice::from_raw_parts(path.cast::<u8>(), pathlen) };
        let (addr, len, _dir) = match crate::bsd::unix_address(path) {
            Ok(address) => address,
            Err(e) => {
                set_errno(e);
                return ptr::null_mut();
            }
        };
        let fd = bsd_connect(libc::AF_UNIX, (&raw const addr).cast(), len, None);
        if fd == -1 {
            return ptr::null_mut();
        }
        with_tls(new_connect_socket(group, fd, kind, options, socket_ext_size), ssl_ctx, None)
    }
}

/// Opens up to `count` candidates of the addresses left; how many it opened.
unsafe fn start_connections(c: *mut us_connecting_socket_t, count: usize) -> usize {
    // SAFETY: the caller passes a live, resolved connecting socket of a live group.
    unsafe {
        let group = (*c).group;
        let mut opened = 0;
        while !(*c).addrinfo_head.is_null() && opened < count {
            let info = &*(*c).addrinfo_head;
            (*c).addrinfo_head = info.ai_next;
            let Some(mut address) = crate::dns::sockaddr_to_std(info.ai_addr) else { continue };
            address.set_port((*c).port);
            let s = connect_resolved(group, (*c).kind, &address, None, (*c).options, (*c).socket_ext_size);
            if s.is_null() {
                continue;
            }
            opened += 1;
            (*s).timeout = (*c).timeout;
            (*s).long_timeout = (*c).long_timeout;
            ptr::copy_nonoverlapping(c.add(1).cast::<u8>(), s.add(1).cast::<u8>(), (*c).socket_ext_size.max(0) as usize);
            (*s).connect_next = (*c).connecting_head;
            (*c).connecting_head = s;
            (*s).connect_state = c;
        }
        opened
    }
}

pub(crate) unsafe fn us_internal_socket_after_resolve(c: *mut us_connecting_socket_t) {
    // SAFETY: the loop passes a connecting socket that its resolver queued.
    unsafe {
        (*c).pending_resolve_callback = false;
        if (*c).closed {
            give_back_request(c, 0);
            us_connecting_socket_free(c);
            return;
        }
        (*(*c).loop_).num_polls -= 1;
        let result = crate::dns::Bun__addrinfo_getRequestResult((*c).addrinfo_req.cast());
        if result.error != 0 {
            // A getaddrinfo code, not an errno: the two overlap, so `error_is_dns` says which.
            (*c).error = result.error;
            (*c).error_is_dns = true;
            us_connecting_socket_close(c);
            return;
        }
        (*c).addrinfo_head = result.entries.cast();
        if start_connections(c, CONCURRENT_CONNECTIONS) == 0 {
            (*c).error = libc::ECONNREFUSED;
            us_connecting_socket_close(c);
        }
    }
}

/// The connect of `s` finished, with `error` or not.
pub(crate) unsafe fn us_internal_socket_after_open(s: *mut us_socket_t, error: c_int) {
    // SAFETY: the loop passes a live semi-socket.
    unsafe {
        let c = (*s).connect_state;
        if error != 0 {
            if c.is_null() {
                us_internal_socket_connect_failed(s, error);
                return;
            }
            let mut next = &raw mut (*c).connecting_head;
            while !(*next).is_null() {
                if *next == s {
                    *next = (*s).connect_next;
                    break;
                }
                next = &raw mut (**next).connect_next;
            }
            us_socket_close(s, LIBUS_SOCKET_CLOSE_CODE_CONNECTION_RESET, ptr::null_mut());
            if (*c).connecting_head.is_null() || (*(*c).connecting_head).connect_next.is_null() {
                let wanted = if (*c).connecting_head.is_null() { CONCURRENT_CONNECTIONS } else { 1 };
                if start_connections(c, wanted) == 0 && (*c).connecting_head.is_null() {
                    (*c).error = libc::ECONNREFUSED;
                    us_connecting_socket_close(c);
                }
            }
            return;
        }
        let loop_ = (*(*s).group).loop_;
        us_poll_change(&mut (*s).p, loop_, LIBUS_SOCKET_READABLE);
        set_nodelay((*s).p.fd, 1);
        us_internal_poll_set_type(&mut (*s).p, POLL_TYPE_SOCKET);
        us_socket_timeout(s, 0);
        if !c.is_null() {
            let mut other = (*c).connecting_head;
            while !other.is_null() {
                let next = (*other).connect_next;
                if other != s {
                    us_socket_close(other, LIBUS_SOCKET_CLOSE_CODE_CONNECTION_RESET, ptr::null_mut());
                }
                other = next;
            }
            // TLS now that the winner is known.
            with_tls(s, (*c).ssl_ctx.cast(), None);
            give_back_request(c, 0);
            us_connecting_socket_free(c);
            (*s).connect_state = ptr::null_mut();
        }
        if (*s).ssl.is_null() {
            us_dispatch_open(s, 1, ptr::null_mut(), 0);
        } else {
            crate::tls::on_open(s, true, ptr::null_mut(), 0);
        }
    }
}

// ── connecting sockets ─────────────────────────────────────────────────────

unsafe fn connecting_detach(c: *mut us_connecting_socket_t) {
    // SAFETY: the caller passes a live connecting socket.
    unsafe {
        if !(*c).group.is_null() {
            group_unlink_connecting((*c).group, c);
            (*c).group = ptr::null_mut();
        }
        if !(*c).ssl_ctx.is_null() {
            crate::tls::SSL_CTX_free(core::mem::replace(&mut (*c).ssl_ctx, ptr::null_mut()));
        }
    }
}

/// Off its group, onto the loop's closed list: it may still be on `dns_ready_head` this iteration.
pub(crate) unsafe fn us_connecting_socket_free(c: *mut us_connecting_socket_t) {
    // SAFETY: the caller passes a live connecting socket.
    unsafe {
        connecting_detach(c);
        let loop_ = (*c).loop_;
        (*c).next = (*loop_).data.closed_connecting_head;
        (*loop_).data.closed_connecting_head = c;
    }
}

pub(crate) unsafe fn us_connecting_socket_dealloc(c: *mut us_connecting_socket_t) {
    // SAFETY: the loop passes a connecting socket from its closed list, which nothing else holds.
    unsafe { xfree(c.cast(), (*c).alloc_size) }
}

/// The ref on the DNS request, given back once; `err` invalidates the cache entry.
unsafe fn give_back_request(c: *mut us_connecting_socket_t, err: c_int) {
    // SAFETY: the caller's connecting socket.
    unsafe {
        let req = core::mem::replace(&mut (*c).addrinfo_req, ptr::null());
        if !req.is_null() {
            crate::dns::Bun__addrinfo_freeRequest(req.cast(), err);
        }
    }
}

/// loop.c's: queues `c` on its loop's `dns_ready_head` and wakes the loop. Any thread.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn us_internal_dns_callback_threadsafe(c: *mut us_connecting_socket_t) {
    // SAFETY: a connecting socket that waits for its result, whose loop outlives it.
    unsafe {
        let loop_ = (*c).loop_;
        let mutex = &raw mut (*loop_).data.mutex;
        Bun__lock(mutex);
        (*c).next = (*loop_).data.dns_ready_head;
        (*loop_).data.dns_ready_head = c;
        Bun__unlock(mutex);
        us_wakeup_loop(loop_);
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn us_connecting_socket_close(c: *mut us_connecting_socket_t) {
    // SAFETY: the caller passes a live connecting socket.
    unsafe {
        if (*c).closed {
            return;
        }
        (*c).closed = true;
        let mut s = (*c).connecting_head;
        while !s.is_null() {
            let next = (*s).connect_next;
            let loop_ = (*(*s).group).loop_;
            group_unlink_socket((*s).group, s);
            us_poll_stop(&mut (*s).p, loop_);
            libc::close((*s).p.fd);
            (*s).next = (*loop_).data.closed_head;
            (*loop_).data.closed_head = s;
            (*s).is_closed = true;
            s = next;
        }
        (*c).connecting_head = ptr::null_mut();
        if (*c).error == 0 {
            (*c).error = libc::ECANCELED;
        }
        if (*c).pending_resolve_callback {
            (*(*c).loop_).num_polls -= 1;
            if crate::dns::Bun__addrinfo_cancel((*c).addrinfo_req.cast(), c) != 0 {
                (*c).pending_resolve_callback = false;
                give_back_request(c, 0);
                us_dispatch_connecting_error(c, (*c).error);
                us_connecting_socket_free(c);
            } else {
                // Already queued: `us_internal_socket_after_resolve` finds `c` closed and frees it.
                us_dispatch_connecting_error(c, (*c).error);
                connecting_detach(c);
            }
            return;
        }
        // A refused connect may mean stale addresses, and a failed lookup is not cached.
        give_back_request(c, c_int::from((*c).error == libc::ECONNREFUSED || (*c).error_is_dns));
        us_dispatch_connecting_error(c, (*c).error);
        us_connecting_socket_free(c);
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn us_connecting_socket_ext(c: *mut us_connecting_socket_t) -> *mut c_void {
    // SAFETY: the ext follows the connecting socket in its allocation.
    unsafe { c.add(1).cast() }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn us_connecting_socket_group(c: *mut us_connecting_socket_t) -> *mut us_socket_group_t {
    // SAFETY: the caller passes a live connecting socket.
    unsafe { (*c).group }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn us_connecting_socket_kind(c: *mut us_connecting_socket_t) -> u8 {
    // SAFETY: as above.
    unsafe { (*c).kind }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn us_connecting_socket_get_error(c: *mut us_connecting_socket_t) -> c_int {
    // SAFETY: as above.
    unsafe { (*c).error }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn us_connecting_socket_get_dns_error(c: *mut us_connecting_socket_t) -> c_int {
    // SAFETY: as above.
    unsafe { if (*c).error_is_dns { (*c).error } else { 0 } }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn us_connecting_socket_get_native_handle(_c: *mut us_connecting_socket_t) -> *mut c_void {
    usize::MAX as *mut c_void
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn us_connecting_socket_is_closed(c: *mut us_connecting_socket_t) -> c_int {
    // SAFETY: as above.
    unsafe { c_int::from((*c).closed) }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn us_connecting_socket_is_shut_down(c: *mut us_connecting_socket_t) -> c_int {
    // SAFETY: as above.
    unsafe { c_int::from((*c).shutdown) }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn us_connecting_socket_shutdown(c: *mut us_connecting_socket_t) {
    // SAFETY: as above.
    unsafe { (*c).shutdown = true };
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn us_connecting_socket_shutdown_read(c: *mut us_connecting_socket_t) {
    // SAFETY: as above.
    unsafe { (*c).shutdown_read = true };
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn us_connecting_socket_timeout(c: *mut us_connecting_socket_t, seconds: c_uint) {
    // SAFETY: the caller passes a live connecting socket of a live group.
    unsafe {
        (*c).timeout = if seconds != 0 {
            ((u32::from((*(*c).group).timestamp) + ((seconds + 3) >> 2)) % 240) as u8
        } else {
            255
        };
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn us_connecting_socket_long_timeout(c: *mut us_connecting_socket_t, minutes: c_uint) {
    // SAFETY: as above.
    unsafe {
        (*c).long_timeout = if minutes != 0 {
            ((u32::from((*(*c).group).long_timestamp) + minutes) % 240) as u8
        } else {
            255
        };
    }
}
