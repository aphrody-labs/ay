//! Sockets made from what the caller has (an fd, a socketpair), sockets moved to another group with a bigger ext
//! (`us_socket_adopt`), and the rest of socket.c's writes and options.

use core::ffi::{c_char, c_int, c_void};
use core::ptr;

use crate::eventing::*;
use crate::socket::*;
use crate::*;

/// `us_socket_adopt`: into `group` as `kind`. A bigger ext moves the socket: the old block is retired (closed, and
/// `prev` points at the new one, which callbacks may still be holding the old pointer to), and freed after the tick.
/// Null for a socket that is closed or shut down.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn us_socket_adopt(
    s: *mut us_socket_t,
    group: *mut us_socket_group_t,
    kind: u8,
    old_ext_size: c_int,
    ext_size: c_int,
) -> *mut us_socket_t {
    // SAFETY: the caller's live socket and initialized group.
    unsafe {
        if (*s).is_closed || us_socket_is_shut_down(s) != 0 {
            return ptr::null_mut();
        }
        let old_group = (*s).group;
        let loop_ = (*old_group).loop_;
        group_unlink_socket(old_group, s);
        let c = (*s).connect_state;
        let mut new_s = s;
        if ext_size != -1 {
            let old_size = core::mem::size_of::<us_socket_t>() + old_ext_size.max(0) as usize;
            let new_size = core::mem::size_of::<us_socket_t>() + ext_size.max(0) as usize;
            if new_size > old_size {
                new_s = us_create_poll(loop_, false, new_size).cast::<us_socket_t>();
                ptr::copy_nonoverlapping(s.cast::<u8>(), new_s.cast::<u8>(), old_size);
                (*new_s).alloc_size = new_size;
                let events = us_poll_events(&(*s).p);
                us_poll_repoint(&mut (*new_s).p, loop_, events);
                us_internal_loop_update_pending_ready_polls(loop_, s.cast(), new_s.cast());
                // The SSL goes with the socket.
                (*s).ssl = ptr::null_mut();
                (*s).is_closed = true;
                (*s).adopted = true;
                (*s).prev = new_s;
                (*s).next = (*loop_).data.closed_head;
                (*loop_).data.closed_head = s;
            }
            if !c.is_null() {
                (*c).connecting_head = new_s;
                (*c).kind = kind;
                crate::context::group_move_connecting(c, group);
            }
        }
        (*new_s).group = group;
        (*new_s).kind = kind;
        (*new_s).timeout = 255;
        (*new_s).long_timeout = 255;
        group_link_socket(group, new_s);
        new_s
    }
}

/// A socket on an fd that is already connected: an IPC channel (`ipc`), a socketpair, or `Bun.connect({ fd })`.
/// It polls for writable at once, so the owner hears on_writable when it can write.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn us_socket_from_fd(
    group: *mut us_socket_group_t,
    kind: u8,
    ssl_ctx: *mut c_void,
    socket_ext_size: c_int,
    fd: c_int,
    options: c_int,
    ipc: c_int,
) -> *mut us_socket_t {
    // SAFETY: the caller's group and fd, which the socket takes.
    unsafe {
        let loop_ = (*group).loop_;
        let size = core::mem::size_of::<us_socket_t>() + socket_ext_size.max(0) as usize;
        let s = us_create_poll(loop_, false, size).cast::<us_socket_t>();
        (*s).alloc_size = size;
        (*s).p.fd = fd;
        (*s).p.poll_type = POLL_TYPE_SOCKET;
        let open_paused = options & LIBUS_SOCKET_OPEN_PAUSED != 0 && ssl_ctx.is_null();
        let readable = if open_paused { 0 } else { LIBUS_SOCKET_READABLE };
        if us_poll_start_rc(&mut (*s).p, loop_, readable | LIBUS_SOCKET_WRITABLE) != 0 {
            us_poll_free(s.cast(), size, loop_);
            return ptr::null_mut();
        }
        (*s).group = group;
        (*s).kind = kind;
        (*s).timeout = 255;
        (*s).long_timeout = 255;
        (*s).allow_half_open = options & LIBUS_SOCKET_ALLOW_HALF_OPEN != 0;
        (*s).is_paused = open_paused;
        (*s).is_ipc = ipc != 0;
        crate::bsd::set_nodelay(fd, 1);
        crate::bsd::set_nonblocking_cloexec(fd);
        group_link_socket(group, s);
        if !ssl_ctx.is_null() {
            crate::tls::attach_client(s, ssl_ctx.cast(), None);
        }
        s
    }
}

/// A connected pair: `fds[0]` becomes a socket of `group`, `fds[1]` is the caller's.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn us_socket_pair(group: *mut us_socket_group_t, kind: u8, socket_ext_size: c_int, fds: *mut c_int) -> *mut us_socket_t {
    // SAFETY: room for two fds.
    unsafe {
        if libc::socketpair(libc::AF_UNIX, libc::SOCK_STREAM, 0, fds) != 0 {
            return ptr::null_mut();
        }
        us_socket_from_fd(group, kind, ptr::null_mut(), socket_ext_size, *fds, 0, 0)
    }
}

/// `us_socket_write` that tells a send that no retry can save (its errno, in `*fatal_write_error`) from a full
/// buffer, which waits for writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn us_socket_write_check_error(
    s: *mut us_socket_t,
    data: *const c_char,
    length: c_int,
    fatal_write_error: *mut c_int,
) -> c_int {
    // SAFETY: the caller's live socket and `length` bytes at `data`.
    unsafe {
        if !fatal_write_error.is_null() {
            *fatal_write_error = 0;
        }
        if (*s).is_closed || us_socket_is_shut_down(s) != 0 {
            return 0;
        }
        if !(*s).ssl.is_null() {
            return us_socket_write(s, data, length);
        }
        let written = loop {
            let r = libc::send(fd_of(s), data.cast(), length.max(0) as usize, crate::bsd::SEND_FLAGS);
            if r == -1 && errno() == libc::EINTR {
                continue;
            }
            break r;
        };
        if written < 0 {
            let fatal = classify_failed_send(s);
            if fatal == 0 {
                (*s).last_write_failed = true;
                rearm_writable(s);
            } else if !fatal_write_error.is_null() {
                *fatal_write_error = fatal;
            }
            return 0;
        }
        (*s).unclassified_send_failures = 0;
        if written != length as isize {
            (*s).last_write_failed = true;
            rearm_writable(s);
        }
        written as c_int
    }
}

/// 0 to retry from writable, else the errno of a send that cannot succeed (`us_internal_classify_failed_send`):
/// the peer-gone errnos at once, any other one after 32 failures in a row.
unsafe fn classify_failed_send(s: *mut us_socket_t) -> c_int {
    let e = errno();
    if matches!(e, libc::EAGAIN | libc::ENOBUFS | libc::ENOMEM | libc::EINTR) {
        return 0;
    }
    let peer_gone = matches!(
        e,
        libc::EPIPE
            | libc::ECONNRESET
            | libc::ECONNABORTED
            | libc::ENOTCONN
            | libc::ETIMEDOUT
            | libc::ENETDOWN
            | libc::ENETUNREACH
            | libc::EHOSTUNREACH
    );
    // SAFETY: the caller's live socket.
    unsafe {
        if !peer_gone && (*s).unclassified_send_failures < 32 {
            (*s).unclassified_send_failures += 1;
            return 0;
        }
    }
    e
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn us_socket_raw_writev(s: *mut us_socket_t, iov: *const libc::iovec, count: c_int) -> c_int {
    // SAFETY: the caller's live socket and `count` iovecs.
    unsafe {
        if (*s).is_closed || us_internal_poll_type(&(*s).p) == POLL_TYPE_SOCKET_SHUT_DOWN {
            return 0;
        }
        let iovs = if count <= 0 { &[][..] } else { core::slice::from_raw_parts(iov, count as usize) };
        let total: usize = iovs.iter().map(|v| v.iov_len).sum();
        let mut msg: libc::msghdr = core::mem::zeroed();
        msg.msg_iov = iov.cast_mut();
        msg.msg_iovlen = count.max(0) as _;
        let written = loop {
            let r = libc::sendmsg(fd_of(s), &msg, crate::bsd::SEND_FLAGS);
            if r == -1 && errno() == libc::EINTR {
                continue;
            }
            break r;
        };
        if written != total as isize {
            (*s).last_write_failed = true;
            rearm_writable(s);
        }
        written.max(0) as c_int
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn us_socket_writev(s: *mut us_socket_t, iov: *const libc::iovec, count: c_int) -> c_int {
    // SAFETY: as above. Through TLS, the parts go one after another.
    unsafe {
        if (*s).ssl.is_null() {
            return us_socket_raw_writev(s, iov, count);
        }
        let mut written = 0;
        for v in core::slice::from_raw_parts(iov, count.max(0) as usize) {
            let n = us_socket_write(s, v.iov_base.cast(), v.iov_len as c_int);
            written += n;
            if n < v.iov_len as c_int {
                break;
            }
        }
        written
    }
}

/// Reads now, as if the poll said readable: for data that arrived while the owner was not reading.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn us_socket_read_now(s: *mut us_socket_t) {
    // SAFETY: the caller's live socket. A callback can tick the loop, which must not free this socket meanwhile.
    unsafe {
        if (*s).is_closed {
            return;
        }
        let mut pfd = libc::pollfd { fd: fd_of(s), events: libc::POLLIN, revents: 0 };
        let hangup = libc::poll(&mut pfd, 1, 0) == 1 && pfd.revents & libc::POLLHUP != 0;
        let loop_ = (*(*s).group).loop_;
        (*loop_).data.tick_depth += 1;
        dispatch_socket(s, 0, if hangup { LIBUS_POLL_HANGUP } else { 0 }, LIBUS_SOCKET_READABLE);
        (*loop_).data.tick_depth -= 1;
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn us_socket_buffer_size(s: *mut us_socket_t, is_recv: c_int, size: c_int, out: *mut c_int) -> c_int {
    let option = if is_recv != 0 { libc::SO_RCVBUF } else { libc::SO_SNDBUF };
    let fd = fd_of(s);
    // SAFETY: int options and the caller's out-int.
    unsafe {
        if size != 0 {
            let err = libc::setsockopt(fd, libc::SOL_SOCKET, option, (&raw const size).cast(), 4);
            if err == 0 {
                *out = size;
            }
            return err;
        }
        let mut value: c_int = 0;
        let mut len = 4 as libc::socklen_t;
        let err = libc::getsockopt(fd, libc::SOL_SOCKET, option, (&raw mut value).cast(), &mut len);
        if err == 0 {
            *out = value;
        }
        err
    }
}

/// IP_TOS or IPV6_TCLASS, by the socket's family: a negative errno if it has neither.
fn tos_option(fd: c_int) -> Result<(c_int, c_int), c_int> {
    // SAFETY: room for any address.
    let mut storage: libc::sockaddr_storage = unsafe { core::mem::zeroed() };
    let mut len = core::mem::size_of::<libc::sockaddr_storage>() as libc::socklen_t;
    // SAFETY: as above.
    if unsafe { libc::getsockname(fd, (&raw mut storage).cast(), &mut len) } != 0 {
        return Err(-errno());
    }
    match c_int::from(storage.ss_family) {
        libc::AF_INET => Ok((libc::IPPROTO_IP, libc::IP_TOS)),
        libc::AF_INET6 => Ok((libc::IPPROTO_IPV6, libc::IPV6_TCLASS)),
        _ => Err(-libc::EINVAL),
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn us_socket_set_tos(s: *mut us_socket_t, tos: c_int) -> c_int {
    // SAFETY: the caller's socket.
    if unsafe { (*s).is_closed } {
        return -libc::EBADF;
    }
    let fd = fd_of(s);
    match tos_option(fd) {
        // SAFETY: an int option.
        Ok((level, option)) => unsafe {
            if libc::setsockopt(fd, level, option, (&raw const tos).cast(), 4) != 0 { -errno() } else { 0 }
        },
        Err(e) => e,
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn us_socket_get_tos(s: *mut us_socket_t) -> c_int {
    // SAFETY: the caller's socket.
    if unsafe { (*s).is_closed } {
        return -libc::EBADF;
    }
    let fd = fd_of(s);
    match tos_option(fd) {
        // SAFETY: an int option.
        Ok((level, option)) => unsafe {
            let mut tos: c_int = 0;
            let mut len = 4 as libc::socklen_t;
            if libc::getsockopt(fd, level, option, (&raw mut tos).cast(), &mut len) != 0 { -errno() } else { tos }
        },
        Err(e) => e,
    }
}

unsafe fn address(s: *const us_socket_t, buf: *mut u8, length: *mut c_int, remote: bool) {
    let (mut ip, mut port, mut v6) = ([0u8; 16], 0, 0);
    // SAFETY: the caller's socket, buffer of `*length` bytes, and out-int.
    unsafe {
        let n = if remote {
            us_get_remote_address_info(ip.as_mut_ptr(), s, ptr::null_mut(), &mut port, &mut v6)
        } else {
            us_get_local_address_info(ip.as_mut_ptr(), s, ptr::null_mut(), &mut port, &mut v6)
        } as c_int;
        if n == 0 || *length < n {
            *length = 0;
            return;
        }
        ptr::copy_nonoverlapping(ip.as_ptr(), buf, n as usize);
        *length = n;
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn us_socket_remote_address(s: *const us_socket_t, buf: *mut u8, length: *mut c_int) {
    // SAFETY: the caller's arguments.
    unsafe { address(s, buf, length, true) }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn us_socket_local_address(s: *const us_socket_t, buf: *mut u8, length: *mut c_int) {
    // SAFETY: the caller's arguments.
    unsafe { address(s, buf, length, false) }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn us_socket_ssl_send_rejected(_s: *const us_socket_t) -> c_int {
    0
}
