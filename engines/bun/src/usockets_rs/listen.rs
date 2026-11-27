//! Listen sockets: context.cpp's listen half, and the accept arm of loop.c's `us_internal_dispatch_ready_poll`.

use core::ffi::{CStr, c_char, c_int, c_uint, c_void};
use core::ptr;

use crate::eventing::*;
use crate::*;

/// `us_internal_init_listen_socket` and `us_internal_start_poll` for a listening fd, which the listener takes.
unsafe fn start_listener(
    group: *mut us_socket_group_t,
    fd: c_int,
    kind: u8,
    ssl_ctx: *mut c_void,
    options: c_int,
    socket_ext_size: c_int,
    error: *mut c_int,
) -> *mut us_listen_socket_t {
    // SAFETY: the caller's initialized group and listening fd.
    unsafe {
        let loop_ = (*group).loop_;
        let size = core::mem::size_of::<us_listen_socket_t>();
        let ls = us_create_poll(loop_, false, size).cast::<us_listen_socket_t>();
        let s = &raw mut (*ls).s;
        (*s).alloc_size = size;
        (*s).p.fd = fd;
        (*s).p.poll_type = POLL_TYPE_SEMI_SOCKET;
        if us_poll_start_rc(&mut (*s).p, loop_, LIBUS_SOCKET_READABLE) != 0 {
            *error = errno();
            crate::bsd::close(fd);
            us_poll_free(ls.cast(), size, loop_);
            return ptr::null_mut();
        }
        (*s).group = group;
        (*s).timeout = 255;
        (*s).long_timeout = 255;
        (*s).allow_half_open = options & LIBUS_SOCKET_ALLOW_HALF_OPEN != 0;
        (*ls).accept_group = group;
        (*ls).accept_kind = kind;
        (*ls).socket_ext_size = socket_ext_size.max(0) as c_uint;
        (*ls).ssl_ctx = ssl_ctx.cast();
        (*ls).server_names = ptr::null_mut();
        if !ssl_ctx.is_null() {
            crate::tls::SSL_CTX_up_ref(ssl_ctx.cast());
        }
        (*ls).accept_paused = options & LIBUS_SOCKET_OPEN_PAUSED != 0 && ssl_ctx.is_null();
        (*ls).next = (*group).head_listen_sockets.cast();
        (*group).head_listen_sockets = ls.cast();
        crate::context::group_touched(group);
        ls
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn us_socket_group_listen(
    group: *mut us_socket_group_t,
    kind: u8,
    ssl_ctx: *mut c_void,
    host: *const c_char,
    port: c_int,
    options: c_int,
    socket_ext_size: c_int,
    error: *mut c_int,
) -> *mut us_listen_socket_t {
    // SAFETY: the caller's group, a NUL-terminated host or null, and an out-int.
    unsafe {
        let host = (!host.is_null()).then(|| CStr::from_ptr(host));
        let fd = crate::bsd::create_listen_socket(host, port, options, &mut *error);
        if fd == -1 {
            return ptr::null_mut();
        }
        let ls = start_listener(group, fd, kind, ssl_ctx, options, socket_ext_size, error);
        if !ls.is_null() && options & LIBUS_LISTEN_DEFER_ACCEPT != 0 {
            (*ls).deferred_accept = crate::bsd::set_defer_accept(fd);
        }
        ls
    }
}

/// A descriptor that the caller already bound, which it may still hold elsewhere: on failure it keeps it.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn us_socket_group_listen_fd(
    group: *mut us_socket_group_t,
    kind: u8,
    ssl_ctx: *mut c_void,
    fd: c_int,
    backlog: c_int,
    options: c_int,
    socket_ext_size: c_int,
    error: *mut c_int,
) -> *mut us_listen_socket_t {
    // SAFETY: the caller's group, fd and out-int.
    unsafe {
        if libc::listen(fd, if backlog > 0 { backlog } else { 512 }) != 0 {
            *error = errno();
            return ptr::null_mut();
        }
        crate::bsd::set_nonblocking_cloexec(fd);
        let ls = start_listener(group, fd, kind, ssl_ctx, options, socket_ext_size, error);
        if !ls.is_null() {
            (*ls).shared = true;
            if options & LIBUS_LISTEN_DEFER_ACCEPT != 0 {
                (*ls).deferred_accept = crate::bsd::set_defer_accept(fd);
            }
        }
        ls
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn us_socket_group_listen_unix(
    group: *mut us_socket_group_t,
    kind: u8,
    ssl_ctx: *mut c_void,
    path: *const c_char,
    pathlen: usize,
    options: c_int,
    socket_ext_size: c_int,
    error: *mut c_int,
) -> *mut us_listen_socket_t {
    // SAFETY: the caller's group, `pathlen` bytes at `path`, and an out-int.
    unsafe {
        let path = if pathlen == 0 { &[][..] } else { core::slice::from_raw_parts(path.cast::<u8>(), pathlen) };
        let fd = crate::bsd::create_listen_socket_unix(path, &mut *error);
        if fd == -1 {
            return ptr::null_mut();
        }
        start_listener(group, fd, kind, ssl_ctx, options, socket_ext_size, error)
    }
}

/// Off the group, onto the loop's closed list: an accept loop can still be reading it.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn us_listen_socket_close(ls: *mut us_listen_socket_t) {
    // SAFETY: the caller's listener.
    unsafe {
        let s = &raw mut (*ls).s;
        if (*s).is_closed {
            return;
        }
        let group = (*ls).accept_group;
        let loop_ = (*(*s).group).loop_;
        us_poll_stop(&mut (*s).p, loop_);
        crate::bsd::close((*s).p.fd);
        crate::sni::listener_free(ls);
        let mut link = (&raw mut (*group).head_listen_sockets).cast::<*mut us_listen_socket_t>();
        while !(*link).is_null() {
            if *link == ls {
                *link = (*ls).next;
                break;
            }
            link = &raw mut (**link).next;
        }
        (*ls).next = ptr::null_mut();
        crate::context::group_maybe_unlink(group);
        (*s).next = (*loop_).data.closed_head;
        (*loop_).data.closed_head = s;
        (*s).is_closed = true;
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn us_listen_socket_ext(ls: *mut us_listen_socket_t) -> *mut c_void {
    // SAFETY: the address after the listener, as in C (whose listeners have no ext either).
    unsafe { ls.add(1).cast() }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn us_socket_group_head_listen_socket(group: *mut us_socket_group_t) -> *mut us_listen_socket_t {
    // SAFETY: the caller's group.
    unsafe { (*group).head_listen_sockets.cast() }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn us_listen_socket_next(ls: *mut us_listen_socket_t) -> *mut us_listen_socket_t {
    // SAFETY: the caller's listener.
    unsafe { (*ls).next }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn us_listen_socket_get_fd(ls: *mut us_listen_socket_t) -> c_int {
    // SAFETY: the caller's listener.
    unsafe { (*ls).s.p.fd }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn us_listen_socket_port(ls: *mut us_listen_socket_t) -> c_int {
    // SAFETY: a listener is a socket.
    unsafe { crate::socket::us_socket_local_port(&raw const (*ls).s) }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn us_listen_socket_group(ls: *mut us_listen_socket_t) -> *mut us_socket_group_t {
    // SAFETY: the caller's listener.
    unsafe { (*ls).accept_group }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn us_listen_socket_set_shared(ls: *mut us_listen_socket_t) {
    // SAFETY: the caller's listener.
    unsafe { (*ls).shared = true };
}

/// A connection or more on a listener: each becomes a socket of `accept_group`, and on_open hears of it.
pub(crate) unsafe fn on_readable(ls: *mut us_listen_socket_t) {
    // SAFETY: the loop's live listener; a handler can close it, which leaves it readable until the tick ends.
    unsafe {
        let group = (*ls).accept_group;
        let loop_ = (*group).loop_;
        while let Some((fd, addr)) = crate::bsd::accept((*ls).s.p.fd) {
            let size = core::mem::size_of::<us_socket_t>() + (*ls).socket_ext_size as usize;
            let mut s = us_create_poll(loop_, false, size).cast::<us_socket_t>();
            (*s).alloc_size = size;
            (*s).p.fd = fd;
            (*s).p.poll_type = POLL_TYPE_SOCKET;
            let events = if (*ls).accept_paused { 0 } else { LIBUS_SOCKET_READABLE };
            if us_poll_start_rc(&mut (*s).p, loop_, events) != 0 {
                // A RST for the peer, not a connection that never answers.
                crate::bsd::close(fd);
                us_poll_free(s.cast(), size, loop_);
                continue;
            }
            (*s).group = group;
            (*s).kind = (*ls).accept_kind;
            (*s).timeout = 255;
            (*s).long_timeout = 255;
            (*s).allow_half_open = (*ls).s.allow_half_open;
            (*s).is_paused = (*ls).accept_paused;
            crate::bsd::set_nodelay(fd, 1);
            crate::socket::group_link_socket(group, s);

            let (mut ip, ip_len) = crate::bsd::ip_bytes(&addr);
            if (*ls).ssl_ctx.is_null() {
                us_dispatch_open(s, 0, ip.as_mut_ptr().cast(), ip_len as c_int);
            } else {
                crate::tls::attach_server(s, (*ls).ssl_ctx, ls);
                crate::tls::on_open(s, false, ip.as_mut_ptr().cast(), ip_len as c_int);
            }
            s = crate::socket::follow_adopted(s);
            // With TCP_DEFER_ACCEPT the request is already there.
            if (*ls).deferred_accept && !s.is_null() && !(*s).is_closed {
                crate::socket::dispatch_socket(s, 0, 0, LIBUS_SOCKET_READABLE);
            }
            if (*ls).s.is_closed {
                break;
            }
        }
    }
}
