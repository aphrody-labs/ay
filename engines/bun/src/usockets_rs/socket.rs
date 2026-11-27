//! Sockets: socket.c, and the socket arm of loop.c's `us_internal_dispatch_ready_poll`.

use core::ffi::{c_char, c_int, c_uint, c_void};
use core::ptr;

use crate::bsd::{SEND_FLAGS, set_nodelay};
use crate::eventing::*;
use crate::*;

pub(crate) fn fd_of(s: *const us_socket_t) -> c_int {
    // SAFETY: callers pass live sockets.
    unsafe { (*s).p.fd }
}

/// The groups' link and unlink of a socket, which arm and disarm the sweep (context.cpp).
pub(crate) unsafe fn group_link_socket(group: *mut us_socket_group_t, s: *mut us_socket_t) {
    // SAFETY: the caller passes a live group and socket.
    unsafe {
        if (*s).is_closed {
            return;
        }
        (*s).group = group;
        (*s).next = (*group).head_sockets;
        (*s).prev = ptr::null_mut();
        if !(*group).head_sockets.is_null() {
            (*(*group).head_sockets).prev = s;
        }
        (*group).head_sockets = s;
        crate::context::group_touched(group);
        us_internal_enable_sweep_timer((*group).loop_);
    }
}

pub(crate) unsafe fn group_unlink_socket(group: *mut us_socket_group_t, s: *mut us_socket_t) {
    // SAFETY: the caller passes a live group and one of its sockets.
    unsafe {
        if s == (*group).iterator {
            (*group).iterator = (*s).next;
        }
        let (prev, next) = ((*s).prev, (*s).next);
        if prev == next {
            (*group).head_sockets = ptr::null_mut();
        } else {
            if prev.is_null() {
                (*group).head_sockets = next;
            } else {
                (*prev).next = next;
            }
            if !next.is_null() {
                (*next).prev = prev;
            }
        }
        us_internal_disable_sweep_timer((*group).loop_);
        crate::context::group_maybe_unlink(group);
    }
}

// ── accessors ──────────────────────────────────────────────────────────────

#[unsafe(no_mangle)]
pub unsafe extern "C" fn us_socket_ext(s: *mut us_socket_t) -> *mut c_void {
    // SAFETY: the ext follows the socket in its allocation.
    unsafe { s.add(1).cast() }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn us_socket_group(s: *mut us_socket_t) -> *mut us_socket_group_t {
    // SAFETY: the caller passes a live socket.
    unsafe { (*s).group }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn us_socket_kind(s: *const us_socket_t) -> u8 {
    // SAFETY: the caller passes a live socket.
    unsafe { (*s).kind }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn us_socket_set_kind(s: *mut us_socket_t, kind: u8) {
    // SAFETY: the caller passes a live socket.
    unsafe { (*s).kind = kind };
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn us_socket_get_fd(s: *const us_socket_t) -> c_int {
    fd_of(s)
}

/// The `SSL` of a TLS socket, else the fd as a pointer.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn us_socket_get_native_handle(s: *mut us_socket_t) -> *mut c_void {
    // SAFETY: the caller passes a live socket.
    unsafe {
        if !(*s).ssl.is_null() {
            return crate::tls::native_handle(s);
        }
    }
    fd_of(s) as usize as *mut c_void
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn us_socket_is_closed(s: *const us_socket_t) -> c_int {
    // SAFETY: the caller passes a socket, which stays readable until the loop frees it.
    unsafe { c_int::from((*s).is_closed) }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn us_socket_is_established(s: *const us_socket_t) -> c_int {
    // SAFETY: the caller passes a live socket.
    unsafe { c_int::from(us_internal_poll_type(&(*s).p) != POLL_TYPE_SEMI_SOCKET) }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn us_socket_is_shut_down(s: *const us_socket_t) -> c_int {
    // SAFETY: the caller passes a live socket.
    unsafe {
        let tls_shut_down = !(*s).ssl.is_null() && crate::tls::is_shut_down(s);
        c_int::from(tls_shut_down || us_internal_poll_type(&(*s).p) == POLL_TYPE_SOCKET_SHUT_DOWN)
    }
}

unsafe fn can_raw_write(s: *const us_socket_t) -> bool {
    // SAFETY: the caller passes a live socket.
    unsafe { !(*s).is_closed && us_internal_poll_type(&(*s).p) != POLL_TYPE_SOCKET_SHUT_DOWN }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn us_socket_get_error(s: *const us_socket_t) -> c_int {
    let mut error: c_int = 0;
    let mut len = core::mem::size_of::<c_int>() as libc::socklen_t;
    // SAFETY: `error` and `len` are what SO_ERROR writes.
    if unsafe { libc::getsockopt(fd_of(s), libc::SOL_SOCKET, libc::SO_ERROR, (&raw mut error).cast(), &mut len) } == -1 {
        return errno();
    }
    error
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn us_socket_queued_input(s: *const us_socket_t) -> c_int {
    // SAFETY: the caller passes a live socket.
    unsafe {
        if (*s).is_closed || us_socket_is_established(s) == 0 {
            return LIBUS_QUEUED_INPUT_NONE;
        }
        let mut byte = 0u8;
        let ret = loop {
            let r = libc::recv(fd_of(s), (&raw mut byte).cast(), 1, libc::MSG_PEEK | libc::MSG_DONTWAIT);
            if r == -1 && errno() == libc::EINTR {
                continue;
            }
            break r;
        };
        match ret {
            r if r > 0 => LIBUS_QUEUED_INPUT_DATA,
            0 => LIBUS_QUEUED_INPUT_EOF,
            _ if matches!(errno(), libc::EAGAIN) => LIBUS_QUEUED_INPUT_NONE,
            _ => LIBUS_QUEUED_INPUT_ERROR,
        }
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn us_socket_timeout(s: *mut us_socket_t, seconds: c_uint) {
    // SAFETY: the caller passes a live socket of a live group.
    unsafe {
        (*s).timeout = if seconds != 0 {
            ((u32::from((*(*s).group).timestamp) + ((seconds + 3) >> 2)) % 240) as u8
        } else {
            255
        };
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn us_socket_long_timeout(s: *mut us_socket_t, minutes: c_uint) {
    // SAFETY: as above.
    unsafe {
        (*s).long_timeout = if minutes != 0 {
            ((u32::from((*(*s).group).long_timestamp) + minutes) % 240) as u8
        } else {
            255
        };
    }
}

/// Uncorks: TCP_CORK off sends what it held back. Only Linux corks.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn us_socket_flush(s: *mut us_socket_t) {
    // SAFETY: the caller passes a live socket.
    #[cfg(any(target_os = "linux", target_os = "android"))]
    if unsafe { us_socket_is_shut_down(s) } == 0 {
        let enabled: c_int = 0;
        // SAFETY: `enabled` is an int to read.
        unsafe { libc::setsockopt(fd_of(s), libc::IPPROTO_TCP, libc::TCP_CORK, (&raw const enabled).cast(), 4) };
    }
    #[cfg(not(any(target_os = "linux", target_os = "android")))]
    let _ = s;
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn us_socket_nodelay(s: *mut us_socket_t, enabled: c_int) {
    // SAFETY: the caller passes a live socket.
    if unsafe { us_socket_is_shut_down(s) } == 0 {
        set_nodelay(fd_of(s), enabled);
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn us_socket_keepalive(s: *mut us_socket_t, enabled: c_int, delay: c_uint) -> c_int {
    // SAFETY: the caller passes a live socket.
    if unsafe { us_socket_is_shut_down(s) } != 0 {
        return 0;
    }
    let fd = fd_of(s);
    #[cfg(not(target_os = "macos"))]
    const TCP_KEEPIDLE: c_int = libc::TCP_KEEPIDLE;
    // macOS's name for it.
    #[cfg(target_os = "macos")]
    const TCP_KEEPIDLE: c_int = libc::TCP_KEEPALIVE;
    let set = |level: c_int, name: c_int, value: c_int| {
        // SAFETY: `value` is an int to read.
        unsafe { libc::setsockopt(fd, level, name, (&raw const value).cast(), 4) == 0 }
    };
    if !set(libc::SOL_SOCKET, libc::SO_KEEPALIVE, enabled) {
        return errno();
    }
    // A delay of 0 keeps the kernel's idle time. Probes every second, ten of them, as on Windows.
    if enabled == 0 || delay == 0 {
        return 0;
    }
    if !set(libc::IPPROTO_TCP, TCP_KEEPIDLE, delay as c_int)
        || !set(libc::IPPROTO_TCP, libc::TCP_KEEPINTVL, 1)
        || !set(libc::IPPROTO_TCP, libc::TCP_KEEPCNT, 10)
    {
        return errno();
    }
    0
}

pub(crate) unsafe fn rearm_writable(s: *mut us_socket_t) {
    // SAFETY: the caller passes a live socket.
    unsafe {
        let readable = if (*s).is_paused || (*s).read_eof { 0 } else { LIBUS_SOCKET_READABLE };
        us_poll_change(&mut (*s).p, (*(*s).group).loop_, LIBUS_SOCKET_WRITABLE | readable);
    }
}

fn send(fd: c_int, data: *const u8, length: usize) -> isize {
    loop {
        // SAFETY: the caller's pointer and length.
        let rc = unsafe { libc::send(fd, data.cast(), length, SEND_FLAGS) };
        if rc == -1 && errno() == libc::EINTR {
            continue;
        }
        return rc;
    }
}

/// TLS's ciphertext to the kernel, below the TLS layer: how much it took. A short send arms writable.
pub(crate) unsafe fn raw_send(s: *mut us_socket_t, data: &[u8]) -> usize {
    // SAFETY: the caller passes a live socket.
    unsafe {
        if !can_raw_write(s) {
            return 0;
        }
        let sent = send(fd_of(s), data.as_ptr(), data.len());
        if sent != data.len() as isize {
            (*s).last_write_failed = true;
            rearm_writable(s);
        }
        sent.max(0) as usize
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn us_socket_write(s: *mut us_socket_t, data: *const c_char, length: c_int) -> c_int {
    // SAFETY: the caller passes a live socket and `length` bytes at `data`.
    unsafe {
        if !(*s).ssl.is_null() {
            let data = if length <= 0 { &[][..] } else { core::slice::from_raw_parts(data.cast::<u8>(), length as usize) };
            return crate::tls::write(s, data);
        }
        if (*s).is_closed || us_socket_is_shut_down(s) != 0 {
            return 0;
        }
        let written = send(fd_of(s), data.cast(), length.max(0) as usize);
        if written != length as isize {
            (*s).last_write_failed = true;
            rearm_writable(s);
        }
        written.max(0) as c_int
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn us_socket_raw_write(s: *mut us_socket_t, data: *const c_char, length: c_int) -> c_int {
    // SAFETY: as above.
    unsafe {
        if !can_raw_write(s) {
            return 0;
        }
        let written = send(fd_of(s), data.cast(), length.max(0) as usize);
        if written >= 0 {
            (*s).unclassified_send_failures = 0;
        }
        if written != length as isize {
            (*s).last_write_failed = true;
            rearm_writable(s);
        }
        written.max(0) as c_int
    }
}

/// `us_socket_write2`: header and payload in one writev.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn us_socket_write2(
    s: *mut us_socket_t,
    header: *const c_char,
    header_length: usize,
    payload: *const c_char,
    payload_length: usize,
) -> c_int {
    // SAFETY: the caller passes a live socket and the two slices.
    unsafe {
        if (*s).is_closed || us_socket_is_shut_down(s) != 0 {
            return 0;
        }
        let iov = [
            libc::iovec { iov_base: header.cast_mut().cast(), iov_len: header_length },
            libc::iovec { iov_base: payload.cast_mut().cast(), iov_len: payload_length },
        ];
        let msg = libc::msghdr {
            msg_name: ptr::null_mut(),
            msg_namelen: 0,
            msg_iov: iov.as_ptr().cast_mut(),
            msg_iovlen: 2,
            msg_control: ptr::null_mut(),
            msg_controllen: 0,
            msg_flags: 0,
        };
        let written = loop {
            let rc = libc::sendmsg(fd_of(s), &msg, SEND_FLAGS);
            if rc == -1 && errno() == libc::EINTR {
                continue;
            }
            break rc;
        };
        if written != (header_length + payload_length) as isize {
            (*s).last_write_failed = true;
            rearm_writable(s);
        }
        written.max(0) as c_int
    }
}

/// The caller's write could not finish: wait for writable, as `us_internal_rearm_writable` does.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn us_socket_sendfile_needs_more(s: *mut us_socket_t) {
    // SAFETY: the caller passes a live socket.
    unsafe {
        if (*s).is_closed {
            return;
        }
        (*s).last_write_failed = true;
        rearm_writable(s);
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn us_socket_pause(s: *mut us_socket_t) {
    // SAFETY: the caller passes a live socket.
    unsafe {
        if (*s).is_paused || (*s).is_closed {
            return;
        }
        let events = if can_raw_write(s) { LIBUS_SOCKET_WRITABLE } else { us_poll_events(&(*s).p) & LIBUS_SOCKET_WRITABLE };
        us_poll_change(&mut (*s).p, (*(*s).group).loop_, events);
        (*s).is_paused = true;
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn us_socket_resume(s: *mut us_socket_t) {
    // SAFETY: the caller passes a live socket.
    unsafe {
        if !(*s).is_paused {
            return;
        }
        (*s).is_paused = false;
        if (*s).is_closed {
            return;
        }
        let mut events = (if (*s).read_eof { 0 } else { LIBUS_SOCKET_READABLE }) | (us_poll_events(&(*s).p) & LIBUS_SOCKET_WRITABLE);
        if us_socket_is_shut_down(s) == 0 {
            events |= LIBUS_SOCKET_WRITABLE;
        }
        if us_poll_change(&mut (*s).p, (*(*s).group).loop_, events) != 0 {
            let err = errno();
            us_internal_socket_close_raw(s, if err > 2 { err } else { libc::ECONNRESET }, ptr::null_mut());
        }
    }
}

/// A FIN; the read side stays open.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn us_socket_shutdown(s: *mut us_socket_t) {
    // SAFETY: the caller passes a live socket.
    unsafe {
        if us_socket_is_established(s) == 0 || !can_raw_write(s) {
            return;
        }
        if !(*s).ssl.is_null() {
            crate::tls::send_close_notify(s);
            return;
        }
        us_internal_poll_set_type(&mut (*s).p, POLL_TYPE_SOCKET_SHUT_DOWN);
        us_poll_change(&mut (*s).p, (*(*s).group).loop_, us_poll_events(&(*s).p) & LIBUS_SOCKET_READABLE);
        libc::shutdown(fd_of(s), libc::SHUT_WR);
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn us_socket_shutdown_read(s: *mut us_socket_t) {
    // SAFETY: idempotent on any fd.
    unsafe { libc::shutdown(fd_of(s), libc::SHUT_RD) };
}

unsafe fn address_info(fd: c_int, buf: *mut u8, port: *mut c_int, is_ipv6: *mut c_int, remote: bool) -> c_uint {
    // SAFETY: `storage` is room for any address; `buf` has 16 bytes.
    unsafe {
        let mut storage: libc::sockaddr_storage = core::mem::zeroed();
        let mut len = core::mem::size_of::<libc::sockaddr_storage>() as libc::socklen_t;
        let addr = (&raw mut storage).cast::<libc::sockaddr>();
        let rc = if remote { libc::getpeername(fd, addr, &mut len) } else { libc::getsockname(fd, addr, &mut len) };
        if rc != 0 {
            return 0;
        }
        match c_int::from(storage.ss_family) {
            libc::AF_INET => {
                let a = &*(&raw const storage).cast::<libc::sockaddr_in>();
                ptr::copy_nonoverlapping((&raw const a.sin_addr).cast::<u8>(), buf, 4);
                *port = c_int::from(u16::from_be(a.sin_port));
                *is_ipv6 = 0;
                4
            }
            libc::AF_INET6 => {
                let a = &*(&raw const storage).cast::<libc::sockaddr_in6>();
                ptr::copy_nonoverlapping((&raw const a.sin6_addr).cast::<u8>(), buf, 16);
                *port = c_int::from(u16::from_be(a.sin6_port));
                *is_ipv6 = 1;
                16
            }
            _ => 0,
        }
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn us_get_remote_address_info(
    buf: *mut u8,
    s: *const us_socket_t,
    _dest: *mut *const c_char,
    port: *mut c_int,
    is_ipv6: *mut c_int,
) -> c_uint {
    // SAFETY: the caller's out-pointers.
    unsafe { address_info(fd_of(s), buf, port, is_ipv6, true) }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn us_get_local_address_info(
    buf: *mut u8,
    s: *const us_socket_t,
    _dest: *mut *const c_char,
    port: *mut c_int,
    is_ipv6: *mut c_int,
) -> c_uint {
    // SAFETY: as above.
    unsafe { address_info(fd_of(s), buf, port, is_ipv6, false) }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn us_socket_local_port(s: *const us_socket_t) -> c_int {
    let (mut buf, mut port, mut v6) = ([0u8; 16], 0, 0);
    // SAFETY: locals to write.
    if unsafe { address_info(fd_of(s), buf.as_mut_ptr(), &mut port, &mut v6, false) } == 0 { -1 } else { port }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn us_socket_remote_port(s: *const us_socket_t) -> c_int {
    let (mut buf, mut port, mut v6) = ([0u8; 16], 0, 0);
    // SAFETY: locals to write.
    if unsafe { address_info(fd_of(s), buf.as_mut_ptr(), &mut port, &mut v6, true) } == 0 { -1 } else { port }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn us_socket_verify_error(s: *mut us_socket_t) -> us_bun_verify_error_t {
    // SAFETY: the caller passes a live socket.
    unsafe {
        if !(*s).ssl.is_null() {
            return crate::tls::verify_error(s);
        }
    }
    us_bun_verify_error_t { error: 0, code: ptr::null(), reason: ptr::null() }
}

// ── close ──────────────────────────────────────────────────────────────────

/// Takes the fd down and tells the holder once: on_close if it opened, on_connect_error if it never did.
unsafe fn close_and_notify(s: *mut us_socket_t, code: c_int, reason: *mut c_void, connect_error: c_int) -> *mut us_socket_t {
    // SAFETY: the caller passes a socket; a closed one goes on the closed list, which the loop frees after the tick.
    unsafe {
        if (*s).is_closed {
            return s;
        }
        let loop_ = (*(*s).group).loop_;
        group_unlink_socket((*s).group, s);
        // kqueue drops the knotes of an fd that closes.
        #[cfg(any(target_os = "linux", target_os = "android"))]
        us_poll_stop(&mut (*s).p, loop_);
        #[cfg(any(target_os = "macos", target_os = "freebsd"))]
        us_internal_loop_update_pending_ready_polls(loop_, s.cast(), ptr::null_mut());
        if code == LIBUS_SOCKET_CLOSE_CODE_CONNECTION_RESET {
            // RST, not TIME_WAIT.
            let l = libc::linger { l_onoff: 1, l_linger: 0 };
            libc::setsockopt(fd_of(s), libc::SOL_SOCKET, libc::SO_LINGER, (&raw const l).cast(), core::mem::size_of::<libc::linger>() as _);
        }
        libc::close(fd_of(s));
        (*s).is_closed = true;
        let mut res = s;
        if us_internal_poll_type(&(*s).p) & POLL_TYPE_SEMI_SOCKET == 0 {
            res = us_dispatch_close(s, code, reason);
        } else if (*s).connect_state.is_null() {
            res = us_dispatch_connect_error(s, connect_error);
        }
        crate::tls::free(s);
        (*s).next = (*loop_).data.closed_head;
        (*loop_).data.closed_head = s;
        res
    }
}

pub(crate) unsafe fn us_internal_socket_close_raw(s: *mut us_socket_t, code: c_int, reason: *mut c_void) -> *mut us_socket_t {
    // SAFETY: the caller's socket.
    unsafe { close_and_notify(s, code, reason, libc::ECANCELED) }
}

pub(crate) unsafe fn us_internal_socket_connect_failed(s: *mut us_socket_t, error: c_int) {
    // SAFETY: the caller's socket.
    unsafe { close_and_notify(s, LIBUS_SOCKET_CLOSE_CODE_CONNECTION_RESET, ptr::null_mut(), error) };
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn us_socket_close(s: *mut us_socket_t, code: c_int, reason: *mut c_void) -> *mut us_socket_t {
    // SAFETY: the caller's socket. A TLS close sends close_notify first, without waiting for the peer's.
    unsafe {
        if !(*s).ssl.is_null() && !(*s).is_closed && code != LIBUS_SOCKET_CLOSE_CODE_CONNECTION_RESET {
            crate::tls::send_close_notify(s);
        }
        us_internal_socket_close_raw(s, code, reason)
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn us_socket_close_now(s: *mut us_socket_t) -> *mut us_socket_t {
    // SAFETY: the caller's socket.
    unsafe { us_socket_close(s, LIBUS_SOCKET_CLOSE_CODE_FAST_SHUTDOWN, ptr::null_mut()) }
}

// ── the readable/writable dispatch ─────────────────────────────────────────

/// loop.c's POLL_TYPE_SOCKET arm, without TLS, IPC and the low-priority queue.
pub(crate) unsafe fn dispatch_socket(mut s: *mut us_socket_t, error: c_int, mut eof: c_int, events: c_int) {
    // SAFETY: the caller passes a live socket of this loop; each handler can close it, which leaves it readable,
    // or adopt it, which leaves the old block pointing at the new one.
    unsafe {
        s = follow_adopted(s);
        let loop_ = (*(*s).group).loop_;
        let hangup = eof & LIBUS_POLL_HANGUP != 0 && error == 0;
        let mut read_fin = false;

        if events & LIBUS_SOCKET_WRITABLE != 0 && error == 0 && !(*s).send_rejected {
            (*s).last_write_failed = false;
            // kqueue's write filter is one-shot: it is gone now, and the poll still reads as before.
            #[cfg(any(target_os = "macos", target_os = "freebsd"))]
            {
                (*s).p.poll_type = us_internal_poll_type(&(*s).p) | ((*s).p.poll_type & POLL_TYPE_POLLING_IN);
            }
            s = if (*s).ssl.is_null() { us_dispatch_writable(s) } else { crate::tls::on_writable(s) };
            s = follow_adopted(s);
            if s.is_null() || (*s).is_closed {
                return;
            }
            if !(*s).last_write_failed || us_socket_is_shut_down(s) != 0 {
                us_poll_change(&mut (*s).p, loop_, us_poll_events(&(*s).p) & LIBUS_SOCKET_READABLE);
            } else {
                #[cfg(any(target_os = "macos", target_os = "freebsd"))]
                us_poll_change(&mut (*s).p, loop_, us_poll_events(&(*s).p) | LIBUS_SOCKET_WRITABLE);
            }
        }

        let drain_for_error = error != 0 && !(*s).read_eof;
        if events & LIBUS_SOCKET_READABLE != 0 || drain_for_error {
            let mut repeat_recv_count = 0;
            loop {
                let buf = (*loop_).data.recv_buf.add(LIBUS_RECV_BUFFER_PADDING);
                let length = if (*s).is_ipc {
                    let (length, fd) = recv_with_fd(fd_of(s), buf);
                    if let Some(fd) = fd {
                        s = follow_adopted(us_dispatch_fd(s, fd));
                        if s.is_null() || (*s).is_closed {
                            break;
                        }
                    }
                    length
                } else {
                    loop {
                        let r = libc::recv(fd_of(s), buf.cast(), LIBUS_RECV_BUFFER_LENGTH, libc::MSG_DONTWAIT);
                        if r == -1 && errno() == libc::EINTR {
                            continue;
                        }
                        break r;
                    }
                };
                if length > 0 {
                    s = if (*s).ssl.is_null() {
                        us_dispatch_data(s, buf.cast(), length as c_int)
                    } else {
                        crate::tls::on_data(s, core::slice::from_raw_parts(buf, length as usize))
                    };
                    s = follow_adopted(s);
                    if s.is_null() {
                        break;
                    }
                    // The hangup or error rode on this event: nothing more will come, so drain what is queued.
                    if !(*s).is_closed && (error != 0 || (!(*s).is_paused && eof != 0)) {
                        continue;
                    }
                    if length as usize >= LIBUS_RECV_BUFFER_LENGTH - 24 * 1024
                        && (*loop_).num_ready_polls < 25
                        && !(*s).is_closed
                        && !(*s).is_paused
                    {
                        repeat_recv_count += 1;
                        if repeat_recv_count <= 10 {
                            continue;
                        }
                    }
                } else if length == 0 {
                    eof = LIBUS_POLL_EOF;
                    read_fin = true;
                } else if errno() != libc::EAGAIN {
                    us_internal_socket_close_raw(s, errno(), ptr::null_mut());
                    return;
                }
                break;
            }
        }

        if !s.is_null() && (*s).send_rejected && !(*s).is_closed {
            us_internal_socket_close_raw(s, libc::ECONNRESET, ptr::null_mut());
            return;
        }

        let eof_deferrable = eof != 0 && !s.is_null() && error == 0 && !(*s).is_closed && !(*s).read_eof;
        if eof_deferrable && (*s).is_paused {
            // EPOLLHUP cannot be masked: leave epoll while paused; resume puts the fd back.
            #[cfg(any(target_os = "linux", target_os = "android"))]
            if hangup {
                us_poll_stop(&mut (*s).p, loop_);
                (*s).p.poll_type = us_internal_poll_type(&(*s).p);
            }
            eof = 0;
        } else if eof_deferrable
            && events & LIBUS_SOCKET_READABLE == 0
            && us_poll_events(&(*s).p) & LIBUS_SOCKET_READABLE != 0
        {
            eof = 0;
        }
        if eof != 0 && error != 0 && !read_fin {
            eof = 0;
        }

        if eof != 0 && !s.is_null() {
            if (*s).is_closed {
                return;
            }
            if us_socket_is_shut_down(s) != 0 {
                us_internal_socket_close_raw(s, LIBUS_SOCKET_CLOSE_CODE_CLEAN_SHUTDOWN, ptr::null_mut());
                return;
            }
            if (*s).allow_half_open && !hangup && (*s).read_eof {
                us_poll_change(&mut (*s).p, loop_, us_poll_events(&(*s).p) & LIBUS_SOCKET_WRITABLE);
            } else if (*s).allow_half_open && !hangup {
                (*s).read_eof = true;
                us_poll_change(&mut (*s).p, loop_, LIBUS_SOCKET_WRITABLE);
                s = follow_adopted(us_dispatch_end(s));
            } else {
                if !(*s).read_eof {
                    s = follow_adopted(us_dispatch_end(s));
                }
                if !s.is_null() {
                    us_internal_socket_close_raw(s, LIBUS_SOCKET_CLOSE_CODE_CLEAN_SHUTDOWN, ptr::null_mut());
                }
                return;
            }
        }

        if error != 0 && !s.is_null() && !(*s).is_closed {
            let socket_error = us_socket_get_error(s);
            us_internal_socket_close_raw(s, if socket_error > 2 { socket_error } else { libc::ECONNRESET }, ptr::null_mut());
        }
    }
}

/// `us_internal_socket_follow_adopted`: a socket that `us_socket_adopt` moved, retired blocks walked to the live one.
pub(crate) unsafe fn follow_adopted(mut s: *mut us_socket_t) -> *mut us_socket_t {
    // SAFETY: retired blocks stay on the closed list until the outermost tick ends.
    unsafe {
        while !s.is_null() && (*s).adopted && !(*s).prev.is_null() {
            s = (*s).prev;
        }
    }
    s
}

/// recvmsg into `buf` with room for one fd: what it read, and the first fd the peer sent. Any other fd that came
/// with it is closed, as the protocol is one per message.
unsafe fn recv_with_fd(fd: c_int, buf: *mut u8) -> (isize, Option<c_int>) {
    const SPACE: usize = 64;
    let mut control = [0u64; SPACE / 8];
    let mut iov = libc::iovec { iov_base: buf.cast(), iov_len: LIBUS_RECV_BUFFER_LENGTH };
    // SAFETY: all-zero is an empty msghdr.
    let mut msg: libc::msghdr = unsafe { core::mem::zeroed() };
    msg.msg_iov = &mut iov;
    msg.msg_iovlen = 1;
    msg.msg_control = control.as_mut_ptr().cast();
    // SAFETY: CMSG_SPACE is arithmetic.
    msg.msg_controllen = unsafe { libc::CMSG_SPACE(core::mem::size_of::<c_int>() as u32) } as _;
    #[cfg(any(target_os = "linux", target_os = "android", target_os = "freebsd"))]
    let flags = libc::MSG_DONTWAIT | libc::MSG_CMSG_CLOEXEC;
    #[cfg(not(any(target_os = "linux", target_os = "android", target_os = "freebsd")))]
    let flags = libc::MSG_DONTWAIT;
    let length = loop {
        // SAFETY: the msghdr points at `buf` and `control`.
        let r = unsafe { libc::recvmsg(fd, &mut msg, flags) };
        if r == -1 && errno() == libc::EINTR {
            continue;
        }
        break r;
    };
    let mut received = None;
    if length > 0 && msg.msg_controllen > 0 {
        // SAFETY: the cmsg walk of what the kernel wrote into `control`.
        unsafe {
            let mut cmsg = libc::CMSG_FIRSTHDR(&msg);
            while !cmsg.is_null() {
                if (*cmsg).cmsg_level == libc::SOL_SOCKET && (*cmsg).cmsg_type == libc::SCM_RIGHTS {
                    let data = libc::CMSG_DATA(cmsg);
                    let count = ((*cmsg).cmsg_len as usize - libc::CMSG_LEN(0) as usize) / core::mem::size_of::<c_int>();
                    for i in 0..count {
                        let passed = data.add(i * core::mem::size_of::<c_int>()).cast::<c_int>().read_unaligned();
                        #[cfg(not(any(target_os = "linux", target_os = "android", target_os = "freebsd")))]
                        libc::fcntl(passed, libc::F_SETFD, libc::FD_CLOEXEC);
                        if received.is_none() {
                            received = Some(passed);
                        } else {
                            libc::close(passed);
                        }
                    }
                }
                cmsg = libc::CMSG_NXTHDR(&msg, cmsg);
            }
        }
    }
    (length, received)
}

/// Sends `data` with `fd` attached (SCM_RIGHTS): what was written, of which less than `length` means the fd did not
/// go. -1 for an error that is not a full buffer.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn us_socket_ipc_write_fd(s: *mut us_socket_t, data: *const c_char, length: c_int, fd: c_int) -> c_int {
    // SAFETY: the caller's live socket and `length` bytes at `data`.
    unsafe {
        if (*s).is_closed || us_socket_is_shut_down(s) != 0 {
            return 0;
        }
        let mut control = [0u64; 8];
        let mut iov = libc::iovec { iov_base: data.cast_mut().cast(), iov_len: length.max(0) as usize };
        let mut msg: libc::msghdr = core::mem::zeroed();
        msg.msg_iov = &mut iov;
        msg.msg_iovlen = 1;
        msg.msg_control = control.as_mut_ptr().cast();
        msg.msg_controllen = libc::CMSG_SPACE(core::mem::size_of::<c_int>() as u32) as _;
        let cmsg = libc::CMSG_FIRSTHDR(&msg);
        (*cmsg).cmsg_level = libc::SOL_SOCKET;
        (*cmsg).cmsg_type = libc::SCM_RIGHTS;
        (*cmsg).cmsg_len = libc::CMSG_LEN(core::mem::size_of::<c_int>() as u32) as _;
        libc::CMSG_DATA(cmsg).cast::<c_int>().write_unaligned(fd);
        let sent = loop {
            let r = libc::sendmsg(fd_of(s), &msg, SEND_FLAGS);
            if r == -1 && errno() == libc::EINTR {
                continue;
            }
            break r;
        };
        if sent < 0 {
            let full = matches!(errno(), libc::EAGAIN | libc::ENOBUFS);
            // The fd has to fit in the buffer in one piece: macOS says EMSGSIZE where Linux says EAGAIN.
            #[cfg(any(target_os = "macos", target_os = "ios"))]
            let full = full || errno() == libc::EMSGSIZE;
            if full {
                (*s).last_write_failed = true;
                us_poll_change(&mut (*s).p, (*(*s).group).loop_, LIBUS_SOCKET_READABLE | LIBUS_SOCKET_WRITABLE);
                return 0;
            }
            return -1;
        }
        if sent != length as isize {
            (*s).last_write_failed = true;
            rearm_writable(s);
        }
        sent as c_int
    }
}
