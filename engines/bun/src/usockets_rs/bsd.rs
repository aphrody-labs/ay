//! bsd.cpp: the socket syscalls under the loop (create, connect, listen, accept, Unix addresses).

use core::ffi::{CStr, c_int};
use core::ptr;
use std::net::SocketAddr;

use crate::*;

#[cfg(any(target_os = "linux", target_os = "android"))]
pub(crate) const SEND_FLAGS: c_int = libc::MSG_NOSIGNAL | libc::MSG_DONTWAIT;
#[cfg(not(any(target_os = "linux", target_os = "android")))]
pub(crate) const SEND_FLAGS: c_int = libc::MSG_DONTWAIT;

/// Close-on-exec and nonblocking; on macOS also SO_NOSIGPIPE, which it has instead of MSG_NOSIGNAL.
pub(crate) fn set_nonblocking_cloexec(fd: c_int) {
    // SAFETY: fcntl and setsockopt on the caller's fd.
    unsafe {
        let flags = libc::fcntl(fd, libc::F_GETFL);
        libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK);
        let fd_flags = libc::fcntl(fd, libc::F_GETFD);
        libc::fcntl(fd, libc::F_SETFD, fd_flags | libc::FD_CLOEXEC);
        #[cfg(any(target_os = "macos", target_os = "ios"))]
        {
            let one: c_int = 1;
            libc::setsockopt(fd, libc::SOL_SOCKET, libc::SO_NOSIGPIPE, (&raw const one).cast(), 4);
        }
    }
}

/// `bsd_create_socket`: -1 and errno on failure.
pub(crate) fn create_socket(domain: c_int, ty: c_int) -> c_int {
    #[cfg(any(target_os = "linux", target_os = "android", target_os = "freebsd"))]
    loop {
        // SAFETY: socket(2) takes integers.
        let fd = unsafe { libc::socket(domain, ty | libc::SOCK_NONBLOCK | libc::SOCK_CLOEXEC, 0) };
        if fd == -1 && errno() == libc::EINTR {
            continue;
        }
        return fd;
    }
    #[cfg(not(any(target_os = "linux", target_os = "android", target_os = "freebsd")))]
    {
        // SAFETY: as above.
        let fd = unsafe { libc::socket(domain, ty, 0) };
        if fd != -1 {
            set_nonblocking_cloexec(fd);
        }
        fd
    }
}

pub(crate) fn close(fd: c_int) {
    // SAFETY: the caller's fd, which it does not use after this.
    unsafe { libc::close(fd) };
}

pub(crate) fn sockaddr_from_std(address: &SocketAddr) -> (libc::sockaddr_storage, libc::socklen_t) {
    // SAFETY: all-zero is an empty sockaddr_storage.
    let mut storage: libc::sockaddr_storage = unsafe { core::mem::zeroed() };
    let len = match address {
        SocketAddr::V4(a) => {
            // SAFETY: sockaddr_storage has room for, and the alignment of, a sockaddr_in.
            let sin = unsafe { &mut *(&raw mut storage).cast::<libc::sockaddr_in>() };
            sin.sin_family = libc::AF_INET as libc::sa_family_t;
            sin.sin_port = a.port().to_be();
            sin.sin_addr.s_addr = u32::from_ne_bytes(a.ip().octets());
            core::mem::size_of::<libc::sockaddr_in>()
        }
        SocketAddr::V6(a) => {
            // SAFETY: as above, for a sockaddr_in6.
            let sin6 = unsafe { &mut *(&raw mut storage).cast::<libc::sockaddr_in6>() };
            sin6.sin6_family = libc::AF_INET6 as libc::sa_family_t;
            sin6.sin6_port = a.port().to_be();
            sin6.sin6_addr.s6_addr = a.ip().octets();
            sin6.sin6_flowinfo = a.flowinfo();
            sin6.sin6_scope_id = a.scope_id();
            core::mem::size_of::<libc::sockaddr_in6>()
        }
    };
    (storage, len as libc::socklen_t)
}

/// The IP of an address as `bsd_addr_get_ip` gives it: 4 or 16 bytes, none for a Unix socket.
pub(crate) fn ip_bytes(storage: &libc::sockaddr_storage) -> ([u8; 16], usize) {
    let mut ip = [0u8; 16];
    match c_int::from(storage.ss_family) {
        libc::AF_INET => {
            // SAFETY: the family says it is a sockaddr_in.
            let sin = unsafe { &*(&raw const *storage).cast::<libc::sockaddr_in>() };
            ip[..4].copy_from_slice(&sin.sin_addr.s_addr.to_ne_bytes());
            (ip, 4)
        }
        libc::AF_INET6 => {
            // SAFETY: the family says it is a sockaddr_in6.
            let sin6 = unsafe { &*(&raw const *storage).cast::<libc::sockaddr_in6>() };
            ip.copy_from_slice(&sin6.sin6_addr.s6_addr);
            (ip, 16)
        }
        _ => (ip, 0),
    }
}

/// A nonblocking connect on a new fd; -1 if it failed at once (errno says why).
pub(crate) fn connect(family: c_int, addr: *const libc::sockaddr, len: libc::socklen_t, local: Option<&SocketAddr>) -> c_int {
    let fd = create_socket(family, libc::SOCK_STREAM);
    if fd == -1 {
        return -1;
    }
    // SAFETY: the caller passes an address of `len` bytes.
    unsafe {
        if let Some(local) = local {
            let on: c_int = 1;
            libc::setsockopt(fd, libc::SOL_SOCKET, libc::SO_REUSEADDR, (&raw const on).cast(), 4);
            let (storage, local_len) = sockaddr_from_std(local);
            if libc::bind(fd, (&raw const storage).cast(), local_len) != 0 {
                let e = errno();
                close(fd);
                set_errno(e);
                return -1;
            }
        }
        loop {
            if libc::connect(fd, addr, len) == 0 {
                return fd;
            }
            match errno() {
                libc::EINPROGRESS | libc::EINTR => return fd,
                e => {
                    close(fd);
                    set_errno(e);
                    return -1;
                }
            }
        }
    }
}

/// An fd to close when the address is done with.
pub(crate) struct DirFd(c_int);

impl Drop for DirFd {
    fn drop(&mut self) {
        let e = errno();
        close(self.0);
        set_errno(e);
    }
}

/// `bsd_create_unix_socket_address`. On Linux a path longer than `sun_path` goes through `/proc/self/fd/<dir>/`,
/// while the returned directory fd lives; a leading NUL is an abstract name. On macOS the caller has to `fchdir`
/// into the directory, which this does not do: a path that long fails with ENAMETOOLONG.
pub(crate) fn unix_address(path: &[u8]) -> Result<(libc::sockaddr_un, libc::socklen_t, Option<DirFd>), c_int> {
    // SAFETY: all-zero is an empty sockaddr_un.
    let mut addr: libc::sockaddr_un = unsafe { core::mem::zeroed() };
    addr.sun_family = libc::AF_UNIX as libc::sa_family_t;
    if path.is_empty() {
        return Err(libc::ENOENT);
    }
    let mut len = core::mem::size_of::<libc::sockaddr_un>() as libc::socklen_t;
    let room = addr.sun_path.len();
    if path.len() < room {
        // SAFETY: `path` fits in `sun_path`.
        unsafe { ptr::copy_nonoverlapping(path.as_ptr().cast(), addr.sun_path.as_mut_ptr(), path.len()) };
        #[cfg(any(target_os = "linux", target_os = "android"))]
        if path[0] == 0 {
            len = (core::mem::offset_of!(libc::sockaddr_un, sun_path) + path.len()) as libc::socklen_t;
        }
        return Ok((addr, len, None));
    }
    #[cfg(any(target_os = "linux", target_os = "android"))]
    if path[0] != 0 {
        let mut dirname_len = path.len();
        while dirname_len > 1 && path[dirname_len - 1] != b'/' {
            dirname_len -= 1;
        }
        if dirname_len < 2 || path.len() - dirname_len + 1 >= room {
            return Err(libc::ENAMETOOLONG);
        }
        let Ok(dirname) = std::ffi::CString::new(&path[..dirname_len]) else { return Err(libc::ENAMETOOLONG) };
        // SAFETY: a NUL-terminated directory name.
        let dir = unsafe { libc::open(dirname.as_ptr(), libc::O_CLOEXEC | libc::O_PATH | libc::O_DIRECTORY, 0o700) };
        if dir == -1 {
            return Err(libc::ENAMETOOLONG);
        }
        let dir = DirFd(dir);
        let mut short = format!("/proc/self/fd/{}/", dir.0).into_bytes();
        short.extend_from_slice(&path[dirname_len..]);
        if short.len() >= room {
            return Err(libc::ENAMETOOLONG);
        }
        // SAFETY: `short` fits in `sun_path`.
        unsafe { ptr::copy_nonoverlapping(short.as_ptr().cast(), addr.sun_path.as_mut_ptr(), short.len()) };
        return Ok((addr, len, Some(dir)));
    }
    let _ = &mut len;
    Err(libc::ENAMETOOLONG)
}

fn bind_and_listen(fd: c_int, addr: *const libc::sockaddr, len: libc::socklen_t, backlog: c_int, error: &mut c_int) -> bool {
    // SAFETY: the caller's fd and address.
    unsafe {
        let mut rc;
        loop {
            rc = libc::bind(fd, addr, len);
            if !(rc == -1 && errno() == libc::EINTR) {
                break;
            }
        }
        if rc == -1 {
            *error = errno();
            return false;
        }
        loop {
            rc = libc::listen(fd, backlog);
            if !(rc == -1 && errno() == libc::EINTR) {
                break;
            }
        }
        *error = errno();
        rc == 0
    }
}

fn set_int_option(fd: c_int, level: c_int, name: c_int, value: c_int) -> c_int {
    // SAFETY: an int option.
    unsafe { libc::setsockopt(fd, level, name, (&raw const value).cast(), 4) }
}

/// `bsd_set_reuse`: REUSE_ADDR is SO_REUSEPORT on the BSDs, where that is what allows a second bind, and a missing
/// SO_REUSEPORT only fails with DISALLOW_REUSE_PORT_FAILURE.
fn set_reuse(fd: c_int, options: c_int) -> c_int {
    if options & LIBUS_LISTEN_REUSE_ADDR != 0 {
        #[cfg(not(any(target_os = "linux", target_os = "android")))]
        let rc = set_int_option(fd, libc::SOL_SOCKET, libc::SO_REUSEPORT, 1);
        #[cfg(any(target_os = "linux", target_os = "android"))]
        let rc = set_int_option(fd, libc::SOL_SOCKET, libc::SO_REUSEADDR, 1);
        if rc != 0 {
            return rc;
        }
    }
    if options & LIBUS_LISTEN_REUSE_PORT != 0 && set_int_option(fd, libc::SOL_SOCKET, libc::SO_REUSEPORT, 1) != 0 {
        let unsupported = matches!(errno(), libc::ENOTSUP | libc::ENOPROTOOPT);
        if unsupported && options & LIBUS_LISTEN_DISALLOW_REUSE_PORT_FAILURE == 0 {
            set_errno(0);
            return 0;
        }
        return -1;
    }
    0
}

/// `bsd_create_listen_socket`: the first IPv6 address of `host` (all when null) that binds, else the first IPv4 one.
pub(crate) fn create_listen_socket(host: Option<&CStr>, port: c_int, options: c_int, error: &mut c_int) -> c_int {
    // SAFETY: all-zero is the empty hints of getaddrinfo(3).
    let mut hints: libc::addrinfo = unsafe { core::mem::zeroed() };
    hints.ai_flags = libc::AI_PASSIVE;
    hints.ai_family = libc::AF_UNSPEC;
    hints.ai_socktype = libc::SOCK_STREAM;
    let service = std::ffi::CString::new(port.to_string()).expect("digits");
    let mut result: *mut libc::addrinfo = ptr::null_mut();
    // SAFETY: NUL-terminated strings, hints and an out-pointer.
    let rc = unsafe { libc::getaddrinfo(host.map_or(ptr::null(), CStr::as_ptr), service.as_ptr(), &hints, &mut result) };
    if rc != 0 {
        return -1;
    }
    let mut found = -1;
    'families: for family in [libc::AF_INET6, libc::AF_INET] {
        let mut a = result;
        while !a.is_null() {
            // SAFETY: a node of getaddrinfo's list.
            let info = unsafe { &*a };
            a = info.ai_next;
            if info.ai_family != family {
                continue;
            }
            let fd = create_socket(info.ai_family, info.ai_socktype);
            if fd == -1 {
                continue;
            }
            // SO_REUSEADDR binds over TIME_WAIT on POSIX (on Windows it would steal a port in use).
            let bound = set_reuse(fd, options) == 0 && {
                set_int_option(fd, libc::SOL_SOCKET, libc::SO_REUSEADDR, 1);
                family != libc::AF_INET6
                    || set_int_option(fd, libc::IPPROTO_IPV6, libc::IPV6_V6ONLY, c_int::from(options & LIBUS_SOCKET_IPV6_ONLY != 0)) == 0
            } && bind_and_listen(fd, info.ai_addr, info.ai_addrlen, 512, error);
            if bound {
                found = fd;
                break 'families;
            }
            close(fd);
        }
    }
    // SAFETY: the list getaddrinfo returned.
    unsafe { libc::freeaddrinfo(result) };
    found
}

pub(crate) fn create_listen_socket_unix(path: &[u8], error: &mut c_int) -> c_int {
    let (addr, len, _dir) = match unix_address(path) {
        Ok(address) => address,
        Err(e) => {
            *error = e;
            set_errno(e);
            return -1;
        }
    };
    let fd = create_socket(libc::AF_UNIX, libc::SOCK_STREAM);
    if fd == -1 {
        return -1;
    }
    if !bind_and_listen(fd, (&raw const addr).cast(), len, 512, error) {
        close(fd);
        return -1;
    }
    fd
}

/// TCP_DEFER_ACCEPT on Linux, the "dataready" accept filter on FreeBSD.
pub(crate) fn set_defer_accept(fd: c_int) -> bool {
    #[cfg(any(target_os = "linux", target_os = "android"))]
    {
        set_int_option(fd, libc::IPPROTO_TCP, libc::TCP_DEFER_ACCEPT, 1) == 0
    }
    #[cfg(not(any(target_os = "linux", target_os = "android")))]
    {
        let _ = fd;
        false
    }
}

/// `bsd_accept_socket`: the next connection, nonblocking and close-on-exec, and its peer's address.
pub(crate) fn accept(fd: c_int) -> Option<(c_int, libc::sockaddr_storage)> {
    loop {
        // SAFETY: room for any address.
        let mut storage: libc::sockaddr_storage = unsafe { core::mem::zeroed() };
        let mut len = core::mem::size_of::<libc::sockaddr_storage>() as libc::socklen_t;
        #[cfg(any(target_os = "linux", target_os = "android", target_os = "freebsd"))]
        // SAFETY: the listener and the address to write.
        let accepted = unsafe {
            libc::accept4(fd, (&raw mut storage).cast(), &mut len, libc::SOCK_CLOEXEC | libc::SOCK_NONBLOCK)
        };
        #[cfg(not(any(target_os = "linux", target_os = "android", target_os = "freebsd")))]
        // SAFETY: as above.
        let accepted = unsafe { libc::accept(fd, (&raw mut storage).cast(), &mut len) };
        if accepted == -1 {
            if errno() == libc::EINTR {
                continue;
            }
            return None;
        }
        #[cfg(not(any(target_os = "linux", target_os = "android", target_os = "freebsd")))]
        set_nonblocking_cloexec(accepted);
        return Some((accepted, storage));
    }
}

pub(crate) fn set_nodelay(fd: c_int, enabled: c_int) {
    set_int_option(fd, libc::IPPROTO_TCP, libc::TCP_NODELAY, enabled);
}
