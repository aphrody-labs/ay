//! eventing/epoll_kqueue.c's kqueue half: kevent64 on macOS, kevent on FreeBSD (as the C's shim calls it). The
//! wakeup is an EVFILT_USER event on both, where the C uses a Mach port on macOS.

use core::ffi::{c_int, c_void};
use core::ptr;

use super::*;

#[cfg(target_os = "macos")]
pub(crate) type ReadyEvent = libc::kevent64_s;
#[cfg(target_os = "freebsd")]
pub(crate) type ReadyEvent = libc::kevent;

/// A poll can be ready for reads and for writes: two filters, two entries.
pub(super) const ENTRIES_PER_POLL: usize = 2;

/// sys/event.h's.
const KEVENT_FLAG_IMMEDIATE: u32 = 0x1;
const KEVENT_FLAG_ERROR_EVENTS: u32 = 0x2;

pub(super) fn ready_ptr(event: &ReadyEvent) -> u64 {
    event.udata as usize as u64
}

pub(super) fn set_ready_ptr(event: &mut ReadyEvent, poll: u64) {
    #[cfg(target_os = "macos")]
    {
        event.udata = poll;
    }
    #[cfg(target_os = "freebsd")]
    {
        event.udata = poll as usize as *mut c_void;
    }
}

/// `EV_SET64`.
fn event(ident: usize, filter: i16, flags: u16, fflags: u32, data: i64, udata: *const c_void) -> ReadyEvent {
    // SAFETY: all-zero is an empty kevent.
    let mut e: ReadyEvent = unsafe { core::mem::zeroed() };
    e.ident = ident as _;
    e.filter = filter as _;
    e.flags = flags as _;
    e.fflags = fflags as _;
    e.data = data as _;
    set_ready_ptr(&mut e, udata as usize as u64);
    e
}

/// kevent64, or on FreeBSD the C's shim of it over kevent: there KEVENT_FLAG_ERROR_EVENTS takes no events (a failed
/// change is -1 and errno), and KEVENT_FLAG_IMMEDIATE is a zero timeout.
unsafe fn kevent(
    kq: c_int,
    changes: *const ReadyEvent,
    nchanges: c_int,
    events: *mut ReadyEvent,
    nevents: c_int,
    flags: u32,
    timeout: *const libc::timespec,
) -> c_int {
    // SAFETY: the caller's buffers of these lengths.
    unsafe {
        #[cfg(target_os = "macos")]
        {
            libc::kevent64(kq, changes, nchanges, events, nevents, flags, timeout)
        }
        #[cfg(target_os = "freebsd")]
        {
            let (events, nevents) = if flags & KEVENT_FLAG_ERROR_EVENTS != 0 { (ptr::null_mut(), 0) } else { (events, nevents) };
            let zero = libc::timespec { tv_sec: 0, tv_nsec: 0 };
            let timeout = if flags & KEVENT_FLAG_IMMEDIATE != 0 && timeout.is_null() { &raw const zero } else { timeout };
            libc::kevent(kq, changes, nchanges, events, nevents, timeout)
        }
    }
}

/// `kqueue_change`: registers the difference between `old_events` and `new_events`. With `keep_read_knote` (socket
/// polls) the read filter stays while reads are off, with an unreachable low-water mark and EV_CLEAR: it is
/// kqueue's EPOLLHUP and EPOLLERR, which report the peer's FIN or RST of a paused socket. The write filter is
/// one-shot.
unsafe fn kqueue_change(kq: c_int, fd: c_int, old_events: c_int, new_events: c_int, udata: *const c_void, keep_read_knote: bool) -> c_int {
    let fd = fd as usize;
    let mut changes = [event(0, 0, 0, 0, 0, ptr::null()); 3];
    let mut n = 0;
    let readable = new_events & LIBUS_SOCKET_READABLE;
    if readable != old_events & LIBUS_SOCKET_READABLE {
        if keep_read_knote {
            changes[n] = event(fd, libc::EVFILT_READ, libc::EV_DELETE, 0, 0, ptr::null());
            n += 1;
            changes[n] = if readable != 0 {
                event(fd, libc::EVFILT_READ, libc::EV_ADD, 0, 0, udata)
            } else {
                event(fd, libc::EVFILT_READ, libc::EV_ADD | libc::EV_CLEAR, libc::NOTE_LOWAT, i32::MAX as i64, udata)
            };
        } else {
            let flags = if readable != 0 { libc::EV_ADD } else { libc::EV_DELETE };
            changes[n] = event(fd, libc::EVFILT_READ, flags, 0, 0, udata);
        }
        n += 1;
    }
    if new_events & LIBUS_SOCKET_WRITABLE != old_events & LIBUS_SOCKET_WRITABLE {
        let flags = if new_events & LIBUS_SOCKET_WRITABLE != 0 { libc::EV_ADD | libc::EV_ONESHOT } else { libc::EV_DELETE };
        changes[n] = event(fd, libc::EVFILT_WRITE, flags, 0, 0, udata);
        n += 1;
    }
    if n == 0 {
        return 0;
    }
    let ret = loop {
        // SAFETY: `n` changes, and room for as many error entries.
        let r = unsafe {
            kevent(kq, changes.as_ptr(), n as c_int, changes.as_mut_ptr(), n as c_int, KEVENT_FLAG_ERROR_EVENTS, ptr::null())
        };
        if r == -1 && errno() == libc::EINTR {
            continue;
        }
        break r;
    };
    // A delete of a filter that is not there (a one-shot write that fired) leaves the fd as asked.
    for change in &changes[..ret.max(0) as usize] {
        if change.flags & libc::EV_DELETE != 0 && change.data == libc::ENOENT as _ {
            continue;
        }
        set_errno(change.data as c_int);
        return 1;
    }
    if ret < 0 { ret } else { 0 }
}

unsafe fn is_socket_poll(p: *const us_poll_t) -> bool {
    // SAFETY: the caller's live poll.
    let t = unsafe { us_internal_poll_type(p) };
    t == POLL_TYPE_SOCKET || t == POLL_TYPE_SOCKET_SHUT_DOWN
}

pub(super) fn create_fd() -> c_int {
    // SAFETY: no arguments to check.
    let kq = unsafe { libc::kqueue() };
    if kq != -1 {
        // SAFETY: the new descriptor.
        unsafe { libc::fcntl(kq, libc::F_SETFD, libc::FD_CLOEXEC) };
    }
    kq
}

/// A socket that starts without reads (LIBUS_SOCKET_OPEN_PAUSED) has the read knote of a paused one.
pub(super) unsafe fn start(p: *mut us_poll_t, loop_: *mut us_loop_t, events: c_int) -> c_int {
    // SAFETY: the caller passes a live poll and its loop.
    unsafe {
        let starts_paused = is_socket_poll(p) && events & LIBUS_SOCKET_READABLE == 0;
        let old = if starts_paused { LIBUS_SOCKET_READABLE } else { 0 };
        kqueue_change((*loop_).fd, (*p).fd, old, events, p.cast(), starts_paused)
    }
}

pub(super) unsafe fn change(p: *mut us_poll_t, loop_: *mut us_loop_t, old_events: c_int, events: c_int) -> c_int {
    // SAFETY: the caller passes a live poll and its loop.
    unsafe { kqueue_change((*loop_).fd, (*p).fd, old_events, events, p.cast(), is_socket_poll(p)) }
}

/// A socket poll has a read knote whether it reads or not.
pub(super) unsafe fn stop(p: *mut us_poll_t, loop_: *mut us_loop_t, old_events: c_int) {
    // SAFETY: the caller passes a live poll and its loop.
    unsafe {
        let registered = if is_socket_poll(p) { old_events | LIBUS_SOCKET_READABLE } else { old_events };
        if registered != 0 {
            kqueue_change((*loop_).fd, (*p).fd, registered, 0, ptr::null(), false);
        }
    }
}

/// `us_poll_resize`'s re-registration: the filters that `p` polls (and a socket's read knote, in its mode) now
/// carry the new block.
pub(super) unsafe fn repoint(p: *mut us_poll_t, loop_: *mut us_loop_t, events: c_int) {
    // SAFETY: the caller passes the poll that it moved, and its loop.
    unsafe {
        let is_socket = is_socket_poll(p);
        let old = if is_socket { !events & LIBUS_SOCKET_READABLE } else { 0 };
        kqueue_change((*loop_).fd, (*p).fd, old, events, p.cast(), is_socket);
    }
}

unsafe fn user_event(loop_: *mut us_loop_t, a: *mut us_internal_async, flags: u16, fflags: u32, with_errors: bool) -> c_int {
    let mut e = event(a as usize, libc::EVFILT_USER, flags, fflags, 0, a.cast());
    let (events, nevents) = if with_errors { (&raw mut e, 1) } else { (ptr::null_mut(), 0) };
    loop {
        // SAFETY: one change, and null or room for its error.
        let r = unsafe { kevent((*loop_).fd, &raw const e, 1, events, nevents, KEVENT_FLAG_ERROR_EVENTS, ptr::null()) };
        if r == -1 && errno() == libc::EINTR {
            continue;
        }
        return r;
    }
}

pub(super) unsafe fn async_init(loop_: *mut us_loop_t, a: *mut us_internal_async) -> bool {
    // SAFETY: the caller's new loop and async.
    unsafe {
        (*a).p.fd = -1;
        user_event(loop_, a, libc::EV_ADD | libc::EV_ENABLE | libc::EV_CLEAR, 0, true) == 0
    }
}

pub(super) unsafe fn async_close(loop_: *mut us_loop_t, a: *mut us_internal_async) {
    // SAFETY: the caller's async, which nothing wakes any more.
    unsafe { user_event(loop_, a, libc::EV_DELETE, 0, true) };
}

/// NOTE_TRIGGER only, with no event list: else this thread could take the wakeup that it posts for the loop.
pub(super) unsafe fn async_wakeup(a: *mut us_internal_async) {
    // SAFETY: the caller's live async; any thread may.
    unsafe { user_event((*a).loop_, a, 0, libc::NOTE_TRIGGER, false) };
}

/// kevent(2) returns EINTR on a caught signal: the rest of the time is waited for. A zero timeout is
/// KEVENT_FLAG_IMMEDIATE, which on XNU skips the wait that a zero timespec still does.
pub(super) unsafe fn wait(loop_: *mut us_loop_t, timeout: Option<libc::timespec>) -> c_int {
    let immediate = timeout.is_some_and(|t| t.tv_sec == 0 && t.tv_nsec == 0);
    let deadline = timeout
        .filter(|t| t.tv_sec != 0 || t.tv_nsec != 0)
        .map(|t| us_internal_monotonic_ns() + t.tv_sec as u64 * 1_000_000_000 + t.tv_nsec as u64);
    let mut remaining = timeout;
    // SAFETY: the caller passes a live loop; the kernel writes at most LIBUS_MAX_READY_POLLS events.
    unsafe {
        let events = (*loop_).ready_polls.0.as_mut_ptr();
        loop {
            let ts = remaining.as_ref().map_or(ptr::null(), |t| t as *const libc::timespec);
            let flags = if immediate { KEVENT_FLAG_IMMEDIATE } else { 0 };
            let ret = kevent((*loop_).fd, ptr::null(), 0, events, LIBUS_MAX_READY_POLLS as c_int, flags, ts);
            if ret != -1 || errno() != libc::EINTR {
                return ret.max(0);
            }
            if let Some(deadline) = deadline {
                let now = us_internal_monotonic_ns();
                if now >= deadline {
                    return 0;
                }
                let left = deadline - now;
                remaining = Some(libc::timespec { tv_sec: (left / 1_000_000_000) as _, tv_nsec: (left % 1_000_000_000) as _ });
            }
        }
    }
}

/// What the filters of one poll said, coalesced: kqueue reports reads and writes as entries of their own.
#[derive(Clone, Copy, Default)]
struct Bits {
    readable: bool,
    writable: bool,
    error: bool,
    eof: bool,
    /// EV_EOF on the write filter: the peer is gone, or our own shutdown.
    send_eof: bool,
    /// The same with a socket error in fflags: the connection died hard.
    send_eof_err: bool,
    /// EV_EOF on the read filter with a socket error: a reset, epoll's EPOLLERR.
    eof_err: bool,
    skip: bool,
}

pub(super) unsafe fn dispatch_ready_polls(loop_: *mut us_loop_t) {
    // SAFETY: the caller passes a live loop whose ready polls were just filled; stopping a poll scrubs it from them.
    unsafe {
        let count = (*loop_).num_ready_polls.max(0) as usize;
        let mut coalesced = [Bits::default(); LIBUS_MAX_READY_POLLS];
        for i in 0..count {
            let e = &(*loop_).ready_polls.0[i];
            let poll = ready_ptr(e);
            if poll == 0 || poll & UNSET_BITS_49_UNTIL_64 != poll {
                coalesced[i].skip = true;
                continue;
            }
            let filter = e.filter as i16;
            let ev_eof = e.flags as u16 & libc::EV_EOF != 0;
            let bits = Bits {
                readable: filter == libc::EVFILT_READ || filter == libc::EVFILT_USER,
                writable: filter == libc::EVFILT_WRITE,
                error: e.flags as u16 & libc::EV_ERROR != 0,
                eof: ev_eof && filter == libc::EVFILT_READ,
                send_eof: ev_eof && filter == libc::EVFILT_WRITE,
                send_eof_err: ev_eof && filter == libc::EVFILT_WRITE && e.fflags != 0,
                eof_err: ev_eof && filter == libc::EVFILT_READ && e.fflags != 0,
                skip: false,
            };
            match (0..i).rev().find(|&j| !coalesced[j].skip && ready_ptr(&(*loop_).ready_polls.0[j]) == poll) {
                Some(j) => {
                    let into = &mut coalesced[j];
                    into.readable |= bits.readable;
                    into.writable |= bits.writable;
                    into.error |= bits.error;
                    into.eof |= bits.eof;
                    into.send_eof |= bits.send_eof;
                    into.send_eof_err |= bits.send_eof_err;
                    into.eof_err |= bits.eof_err;
                    coalesced[i].skip = true;
                }
                None => coalesced[i] = bits,
            }
        }

        (*loop_).current_ready_poll = 0;
        while (*loop_).current_ready_poll < (*loop_).num_ready_polls {
            let index = (*loop_).current_ready_poll as usize;
            let poll = ready_ptr(&(*loop_).ready_polls.0[index]);
            if poll != 0 {
                if poll & UNSET_BITS_49_UNTIL_64 != poll {
                    Bun__internal_dispatch_ready_poll(loop_, poll as *mut c_void);
                } else if !coalesced[index].skip {
                    dispatch_coalesced(poll as *mut us_poll_t, coalesced[index]);
                }
            }
            (*loop_).current_ready_poll += 1;
        }
    }
}

unsafe fn dispatch_coalesced(p: *mut us_poll_t, bits: Bits) {
    // SAFETY: a live poll of this crate.
    unsafe {
        let mut events = (if bits.readable { LIBUS_SOCKET_READABLE } else { 0 }) | (if bits.writable { LIBUS_SOCKET_WRITABLE } else { 0 });
        let mut error = c_int::from(bits.error);
        let mut eof = if bits.eof { LIBUS_POLL_EOF } else { 0 };
        if bits.send_eof || bits.eof_err {
            let t = us_internal_poll_type(p);
            if t == POLL_TYPE_SOCKET || t == POLL_TYPE_SEMI_SOCKET {
                let s = p.cast::<us_socket_t>();
                if bits.send_eof
                    && !bits.send_eof_err
                    && !bits.eof_err
                    && t == POLL_TYPE_SOCKET
                    && !((*s).last_write_failed && !(*s).is_paused)
                {
                    // No socket error: with the read side down too, an AF_UNIX peer's close (epoll's EPOLLHUP).
                    if (*s).read_eof || eof != 0 {
                        eof = LIBUS_POLL_HANGUP;
                    }
                } else {
                    error = 1;
                }
            } else if t == POLL_TYPE_SOCKET_SHUT_DOWN && (bits.send_eof_err || bits.eof_err) {
                error = 1;
            }
        }
        events &= us_poll_events(p);
        if events != 0 || error != 0 || eof != 0 {
            us_internal_dispatch_ready_poll(p, error, eof, events);
        }
    }
}
