//! eventing/epoll_kqueue.c's epoll half: the set, the eventfd that wakes the loop, and epoll_pwait2.

use core::ffi::{c_int, c_void};
use core::ptr;
use core::sync::atomic::Ordering;

use super::*;

pub(crate) type ReadyEvent = libc::epoll_event;

/// The entries that one poll can have among the ready polls.
pub(super) const ENTRIES_PER_POLL: usize = 1;

/// The kernel's `sigset_t` (`_NSIG / 8`), not glibc's.
const KERNEL_SIGSET_SIZE: usize = 8;

pub(super) fn ready_ptr(event: &ReadyEvent) -> u64 {
    event.u64
}

pub(super) fn set_ready_ptr(event: &mut ReadyEvent, poll: u64) {
    event.u64 = poll;
}

fn epoll_events(events: c_int) -> u32 {
    let mut e = events as u32;
    if events & (LIBUS_SOCKET_READABLE | LIBUS_SOCKET_WRITABLE) == 0 {
        // Neither direction: EPOLLHUP and EPOLLERR still report, which is what closes a half-open socket.
        e |= (libc::EPOLLHUP | libc::EPOLLERR) as u32;
    }
    e
}

fn epoll_ctl(epfd: c_int, op: c_int, fd: c_int, event: &mut libc::epoll_event) -> c_int {
    loop {
        // SAFETY: `event` is an epoll_event to read.
        let rc = unsafe { libc::epoll_ctl(epfd, op, fd, event) };
        if rc == -1 && errno() == libc::EINTR {
            continue;
        }
        return rc;
    }
}

pub(super) fn create_fd() -> c_int {
    // SAFETY: no arguments to check.
    unsafe { libc::epoll_create1(libc::EPOLL_CLOEXEC) }
}

pub(super) unsafe fn start(p: *mut us_poll_t, loop_: *mut us_loop_t, events: c_int) -> c_int {
    // SAFETY: the caller passes a live poll and its loop.
    unsafe {
        let mut event = libc::epoll_event { events: epoll_events(events), u64: p as u64 };
        epoll_ctl((*loop_).fd, libc::EPOLL_CTL_ADD, (*p).fd, &mut event)
    }
}

pub(super) unsafe fn change(p: *mut us_poll_t, loop_: *mut us_loop_t, _old_events: c_int, events: c_int) -> c_int {
    // SAFETY: the caller passes a live poll and its loop.
    unsafe {
        let mut event = libc::epoll_event { events: epoll_events(events), u64: p as u64 };
        let rc = epoll_ctl((*loop_).fd, libc::EPOLL_CTL_MOD, (*p).fd, &mut event);
        // A paused socket that hung up left epoll; this is its resume coming back.
        if rc == -1 && errno() == libc::ENOENT {
            return start(p, loop_, events);
        }
        rc
    }
}

pub(super) unsafe fn stop(p: *mut us_poll_t, loop_: *mut us_loop_t, _old_events: c_int) {
    // SAFETY: the caller passes a live poll and its loop.
    unsafe {
        let mut event = libc::epoll_event { events: 0, u64: 0 };
        epoll_ctl((*loop_).fd, libc::EPOLL_CTL_DEL, (*p).fd, &mut event);
    }
}

/// `us_poll_resize`'s re-registration: the entry of the fd points at `p`, its new block.
pub(super) unsafe fn repoint(p: *mut us_poll_t, loop_: *mut us_loop_t, events: c_int) {
    // SAFETY: the caller passes the poll that it moved, and its loop.
    unsafe {
        let mut event = libc::epoll_event { events: epoll_events(events), u64: p as u64 };
        if epoll_ctl((*loop_).fd, libc::EPOLL_CTL_MOD, (*p).fd, &mut event) == -1 && errno() == libc::ENOENT {
            start(p, loop_, events);
        }
    }
}

/// The wakeup: an eventfd, edge-triggered, so that a wakeup is never read.
pub(super) unsafe fn async_init(loop_: *mut us_loop_t, a: *mut us_internal_async) -> bool {
    // SAFETY: the caller's new loop and async.
    unsafe {
        let efd = libc::eventfd(0, libc::EFD_NONBLOCK | libc::EFD_CLOEXEC);
        if efd == -1 {
            return false;
        }
        (*a).p.fd = efd;
        let mut event = libc::epoll_event { events: (libc::EPOLLIN | libc::EPOLLET) as u32, u64: a as u64 };
        epoll_ctl((*loop_).fd, libc::EPOLL_CTL_ADD, efd, &mut event);
        true
    }
}

pub(super) unsafe fn async_close(_loop: *mut us_loop_t, a: *mut us_internal_async) {
    // SAFETY: the caller's async, which nothing wakes any more.
    unsafe { libc::close((*a).p.fd) };
}

pub(super) unsafe fn async_wakeup(a: *mut us_internal_async) {
    // SAFETY: the caller's live async; any thread may.
    unsafe {
        let fd = (*a).p.fd;
        loop {
            let val: u64 = 1;
            if libc::write(fd, (&raw const val).cast(), 8) >= 0 {
                return;
            }
            match errno() {
                libc::EINTR => continue,
                libc::EAGAIN => {
                    let mut drained: u64 = 0;
                    if libc::read(fd, (&raw mut drained).cast(), 8) > 0 || matches!(errno(), libc::EAGAIN | libc::EINTR) {
                        continue;
                    }
                    return;
                }
                _ => return,
            }
        }
    }
}

/// epoll_pwait2 (ns timeouts), or epoll_pwait with milliseconds rounded up where the kernel lacks it. EINTR waits
/// for the rest of the time.
pub(super) unsafe fn wait(loop_: *mut us_loop_t, timeout: Option<libc::timespec>) -> c_int {
    static HAS_EPOLL_PWAIT2: core::sync::atomic::AtomicI8 = core::sync::atomic::AtomicI8::new(-1);
    let deadline = timeout
        .filter(|t| t.tv_sec != 0 || t.tv_nsec != 0)
        .map(|t| us_internal_monotonic_ns() + t.tv_sec as u64 * 1_000_000_000 + t.tv_nsec as u64);
    let mut remaining = timeout;
    // SAFETY: the caller passes a live loop; the kernel writes at most LIBUS_MAX_READY_POLLS events.
    unsafe {
        let events = (*loop_).ready_polls.0.as_mut_ptr();
        loop {
            let ret = if HAS_EPOLL_PWAIT2.load(Ordering::Relaxed) != 0 {
                let mut mask: libc::sigset_t = core::mem::zeroed();
                libc::sigemptyset(&mut mask);
                let ts = remaining.as_ref().map_or(ptr::null(), |t| t as *const libc::timespec);
                let r = libc::syscall(
                    libc::SYS_epoll_pwait2,
                    (*loop_).fd,
                    events,
                    LIBUS_MAX_READY_POLLS as c_int,
                    ts,
                    &mask as *const libc::sigset_t,
                    KERNEL_SIGSET_SIZE,
                ) as c_int;
                if r == -1 && matches!(errno(), libc::ENOSYS | libc::EPERM | libc::EOPNOTSUPP | libc::EACCES | libc::EFAULT) {
                    HAS_EPOLL_PWAIT2.store(0, Ordering::Relaxed);
                    continue;
                }
                r
            } else {
                let ms = remaining.map_or(-1, |t| {
                    let ns = t.tv_sec as i128 * 1_000_000_000 + t.tv_nsec as i128;
                    ((ns.max(0) + 999_999) / 1_000_000).min(i32::MAX as i128) as c_int
                });
                libc::epoll_wait((*loop_).fd, events, LIBUS_MAX_READY_POLLS as c_int, ms)
            };
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

pub(super) unsafe fn dispatch_ready_polls(loop_: *mut us_loop_t) {
    // SAFETY: the caller passes a live loop whose ready polls were just filled; stopping a poll scrubs it from them.
    unsafe {
        (*loop_).current_ready_poll = 0;
        while (*loop_).current_ready_poll < (*loop_).num_ready_polls {
            let ready = (*loop_).ready_polls.0[(*loop_).current_ready_poll as usize];
            let poll = ready.u64;
            if poll != 0 {
                if poll & UNSET_BITS_49_UNTIL_64 != poll {
                    Bun__internal_dispatch_ready_poll(loop_, poll as *mut c_void);
                } else {
                    let p = poll as *mut us_poll_t;
                    let mut events = ready.events as c_int;
                    let error = c_int::from(events & libc::EPOLLERR != 0);
                    let eof = if events & libc::EPOLLHUP != 0 { LIBUS_POLL_HANGUP } else { 0 };
                    events &= us_poll_events(p);
                    if events != 0 || error != 0 || eof != 0 {
                        us_internal_dispatch_ready_poll(p, error, eof, events);
                    }
                }
            }
            (*loop_).current_ready_poll += 1;
        }
    }
}
