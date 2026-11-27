//! The loop: eventing/epoll_kqueue.c and the loop half of loop.c, on `epoll` (Linux) or `kqueue` (macOS, FreeBSD).

use core::ffi::{c_int, c_uint, c_void};
use core::ptr;
use core::sync::atomic::Ordering;

use crate::*;

#[cfg(any(target_os = "linux", target_os = "android"))]
#[path = "eventing/epoll.rs"]
mod backend;
#[cfg(any(target_os = "macos", target_os = "freebsd"))]
#[path = "eventing/kqueue.rs"]
mod backend;

pub(crate) use backend::ReadyEvent;

/// `struct us_internal_callback_t` of the loop's wakeup.
#[repr(C, align(16))]
pub struct us_internal_async {
    p: us_poll_t,
    loop_: *mut us_loop_t,
    cb: Option<LoopCb>,
}

const UNSET_BITS_49_UNTIL_64: u64 = 0x0000_FFFF_FFFF_FFFF;
const LIBUS_TIMEOUT_GRANULARITY_NS: i64 = LIBUS_TIMEOUT_GRANULARITY * 1_000_000_000;

pub(crate) fn us_internal_monotonic_ns() -> u64 {
    let mut ts = libc::timespec { tv_sec: 0, tv_nsec: 0 };
    // SAFETY: `ts` is a timespec to write.
    unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut ts) };
    ts.tv_sec as u64 * 1_000_000_000 + ts.tv_nsec as u64
}

// ── polls ──────────────────────────────────────────────────────────────────

pub(crate) unsafe fn us_poll_events(p: *const us_poll_t) -> c_int {
    // SAFETY: the caller passes a live poll.
    let t = unsafe { (*p).poll_type };
    (if t & POLL_TYPE_POLLING_IN != 0 { LIBUS_SOCKET_READABLE } else { 0 })
        | (if t & POLL_TYPE_POLLING_OUT != 0 { LIBUS_SOCKET_WRITABLE } else { 0 })
}

pub(crate) unsafe fn us_internal_poll_type(p: *const us_poll_t) -> u8 {
    // SAFETY: the caller passes a live poll.
    unsafe { (*p).poll_type & POLL_TYPE_KIND_MASK }
}

pub(crate) unsafe fn us_internal_poll_set_type(p: *mut us_poll_t, poll_type: u8) {
    // SAFETY: the caller passes a live poll.
    unsafe { (*p).poll_type = poll_type | ((*p).poll_type & POLL_TYPE_POLLING_MASK) };
}

fn polling_bits(events: c_int) -> u8 {
    (if events & LIBUS_SOCKET_READABLE != 0 { POLL_TYPE_POLLING_IN } else { 0 })
        | (if events & LIBUS_SOCKET_WRITABLE != 0 { POLL_TYPE_POLLING_OUT } else { 0 })
}

/// Counts in `num_polls` unless `fallthrough`.
pub(crate) unsafe fn us_create_poll(loop_: *mut us_loop_t, fallthrough: bool, size: usize) -> *mut u8 {
    if !fallthrough {
        // SAFETY: the caller passes a live loop.
        unsafe { (*loop_).num_polls += 1 };
    }
    xcalloc(size)
}

pub(crate) unsafe fn us_poll_free(p: *mut u8, size: usize, loop_: *mut us_loop_t) {
    // SAFETY: the caller passes what `us_create_poll` gave it, and its loop.
    unsafe {
        (*loop_).num_polls -= 1;
        xfree(p, size);
    }
}

pub(crate) unsafe fn us_poll_start_rc(p: *mut us_poll_t, loop_: *mut us_loop_t, events: c_int) -> c_int {
    // SAFETY: the caller passes a live poll and its loop.
    unsafe {
        (*p).poll_type = us_internal_poll_type(p) | polling_bits(events);
        backend::start(p, loop_, events)
    }
}

pub(crate) unsafe fn us_poll_change(p: *mut us_poll_t, loop_: *mut us_loop_t, events: c_int) -> c_int {
    // SAFETY: the caller passes a live poll and its loop.
    unsafe {
        let old_events = us_poll_events(p);
        if old_events == events {
            return 0;
        }
        (*p).poll_type = us_internal_poll_type(p) | polling_bits(events);
        backend::change(p, loop_, old_events, events)
    }
}

pub(crate) unsafe fn us_poll_stop(p: *mut us_poll_t, loop_: *mut us_loop_t) {
    // SAFETY: the caller passes a live poll and its loop.
    unsafe {
        backend::stop(p, loop_, us_poll_events(p));
        us_internal_loop_update_pending_ready_polls(loop_, p.cast(), ptr::null_mut());
    }
}

/// `us_poll_resize`'s second half: `p` is the new block of a poll that kept its fd and its events.
pub(crate) unsafe fn us_poll_repoint(p: *mut us_poll_t, loop_: *mut us_loop_t, events: c_int) {
    // SAFETY: the caller passes the poll that it moved, and its loop.
    unsafe {
        (*p).poll_type = us_internal_poll_type(p) | polling_bits(events);
        backend::repoint(p, loop_, events);
    }
}

/// Points what is left of this iteration's ready polls at `new_poll` instead of `old_poll` (null: skip it).
pub(crate) unsafe fn us_internal_loop_update_pending_ready_polls(loop_: *mut us_loop_t, old_poll: *mut c_void, new_poll: *mut c_void) {
    // SAFETY: the caller passes a live loop.
    unsafe {
        let start = (*loop_).current_ready_poll.max(0) as usize;
        let end = (*loop_).num_ready_polls.max(0) as usize;
        let mut remaining = backend::ENTRIES_PER_POLL;
        for i in start..end {
            if remaining == 0 {
                break;
            }
            if backend::ready_ptr(&(*loop_).ready_polls.0[i]) == old_poll as u64 {
                backend::set_ready_ptr(&mut (*loop_).ready_polls.0[i], new_poll as u64);
                remaining -= 1;
            }
        }
    }
}

// ── the loop ───────────────────────────────────────────────────────────────

#[unsafe(no_mangle)]
pub unsafe extern "C" fn us_create_loop(
    _hint: *mut c_void,
    wakeup_cb: Option<LoopCb>,
    pre_cb: Option<LoopCb>,
    post_cb: Option<LoopCb>,
    ext_size: c_uint,
) -> *mut us_loop_t {
    let size = core::mem::size_of::<us_loop_t>() + ext_size as usize;
    let loop_ = xcalloc(size).cast::<us_loop_t>();
    // SAFETY: `loop_` is zeroed memory for a loop, which zero is a valid state of.
    unsafe {
        (*loop_).fd = backend::create_fd();
        if (*loop_).fd == -1 {
            xfree(loop_.cast(), size);
            return ptr::null_mut();
        }
        let data = &mut (*loop_).data;
        data.sweep_next_tick_ns = -1;
        data.recv_buf = xcalloc(LIBUS_RECV_BUFFER_LENGTH + LIBUS_RECV_BUFFER_PADDING * 2);
        data.pre_cb = pre_cb;
        data.post_cb = post_cb;
        let a = us_create_poll(loop_, true, core::mem::size_of::<us_internal_async>()).cast::<us_internal_async>();
        (*a).p.poll_type = POLL_TYPE_CALLBACK | POLL_TYPE_POLLING_IN;
        (*a).loop_ = loop_;
        (*a).cb = wakeup_cb;
        if !backend::async_init(loop_, a) {
            xfree(a.cast(), core::mem::size_of::<us_internal_async>());
            xfree(data.recv_buf, LIBUS_RECV_BUFFER_LENGTH + LIBUS_RECV_BUFFER_PADDING * 2);
            libc::close((*loop_).fd);
            xfree(loop_.cast(), size);
            return ptr::null_mut();
        }
        data.wakeup_async = a;
    }
    loop_
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn us_loop_free(loop_: *mut us_loop_t) {
    // SAFETY: the caller passes a loop from `us_create_loop` that nothing uses any more.
    unsafe {
        us_internal_free_closed_sockets(loop_);
        let a = (*loop_).data.wakeup_async;
        backend::async_close(loop_, a);
        xfree(a.cast(), core::mem::size_of::<us_internal_async>());
        xfree((*loop_).data.recv_buf, LIBUS_RECV_BUFFER_LENGTH + LIBUS_RECV_BUFFER_PADDING * 2);
        libc::close((*loop_).fd);
        xfree(loop_.cast(), core::mem::size_of::<us_loop_t>());
    }
}

thread_local! {
    static THREAD_LOOP: core::cell::Cell<*mut us_loop_t> = const { core::cell::Cell::new(ptr::null_mut()) };
}

unsafe extern "C" fn noop(_: *mut us_loop_t) {}

/// The thread's loop, made on first use (libuwsockets.cpp's `uWS::Loop::get()`; uWS's own pre/post hooks have no
/// users here).
#[unsafe(no_mangle)]
pub extern "C" fn uws_get_loop() -> *mut us_loop_t {
    THREAD_LOOP.with(|cell| {
        if cell.get().is_null() {
            // SAFETY: no hint, no ext.
            let loop_ = unsafe { us_create_loop(ptr::null_mut(), Some(noop), Some(noop), Some(noop), 0) };
            assert!(!loop_.is_null(), "uws_get_loop: the loop's descriptor or its wakeup failed (errno {})", errno());
            cell.set(loop_);
        }
        cell.get()
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn us_wakeup_loop(loop_: *mut us_loop_t) {
    // SAFETY: the caller passes a live loop; any thread may.
    unsafe {
        (*loop_).pending_wakeups.fetch_add(1, Ordering::Release);
        backend::async_wakeup((*loop_).data.wakeup_async);
    }
}

/// A poll that runs `cb(loop)` while `fd`, which the caller keeps open, is readable: the `us_internal_callback_t`
/// that the loop's wakeup is, on a descriptor of the caller's (the epoll set of another reactor, for one). It is a
/// poll of the loop, so a tick waits for it; it is not `active`, so it keeps nothing alive. Null if epoll refuses
/// the descriptor.
///
/// # Safety
/// `loop_` is this thread's live loop and `fd` an open descriptor that outlives the poll.
pub unsafe fn us_internal_create_callback_poll(loop_: *mut us_loop_t, fd: c_int, cb: LoopCb) -> *mut us_internal_async {
    let size = core::mem::size_of::<us_internal_async>();
    // SAFETY: the contract of this function; the poll is zeroed memory of `size` bytes.
    unsafe {
        let a = us_create_poll(loop_, false, size).cast::<us_internal_async>();
        (*a).p.fd = fd;
        (*a).p.poll_type = POLL_TYPE_CALLBACK;
        (*a).loop_ = loop_;
        (*a).cb = Some(cb);
        if us_poll_start_rc(&raw mut (*a).p, loop_, LIBUS_SOCKET_READABLE) == -1 {
            us_poll_free(a.cast(), size, loop_);
            return ptr::null_mut();
        }
        a
    }
}

/// Stops and frees what [`us_internal_create_callback_poll`] made; the descriptor stays open.
///
/// # Safety
/// `a` is a live callback poll of this thread's loop.
pub unsafe fn us_internal_callback_poll_free(a: *mut us_internal_async) {
    // SAFETY: the contract of this function.
    unsafe {
        let loop_ = (*a).loop_;
        us_poll_stop(&raw mut (*a).p, loop_);
        us_poll_free(a.cast(), core::mem::size_of::<us_internal_async>(), loop_);
    }
}

// ── the sweep ──────────────────────────────────────────────────────────────

#[unsafe(no_mangle)]
pub unsafe extern "C" fn us_internal_enable_sweep_timer(loop_: *mut us_loop_t) {
    // SAFETY: the caller passes a live loop.
    unsafe {
        (*loop_).data.sweep_timer_count += 1;
        if (*loop_).data.sweep_timer_count == 1 {
            (*loop_).data.sweep_next_tick_ns = us_internal_monotonic_ns() as i64 + LIBUS_TIMEOUT_GRANULARITY_NS;
        }
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn us_internal_disable_sweep_timer(loop_: *mut us_loop_t) {
    // SAFETY: the caller passes a live loop.
    unsafe {
        (*loop_).data.sweep_timer_count -= 1;
        if (*loop_).data.sweep_timer_count == 0 {
            (*loop_).data.sweep_next_tick_ns = -1;
        }
    }
}

unsafe fn us_internal_sweep_timeout_ns(loop_: *mut us_loop_t) -> i64 {
    // SAFETY: the caller passes a live loop.
    let next = unsafe { (*loop_).data.sweep_next_tick_ns };
    if next < 0 {
        return -1;
    }
    (next - us_internal_monotonic_ns() as i64).max(0)
}

unsafe fn us_internal_sweep_if_due(loop_: *mut us_loop_t) {
    // SAFETY: the caller passes a live loop.
    unsafe {
        if (*loop_).data.sweep_next_tick_ns < 0 {
            return;
        }
        let now = us_internal_monotonic_ns() as i64;
        if now < (*loop_).data.sweep_next_tick_ns {
            return;
        }
        // Re-armed first: a timeout handler may unlink the last socket and disarm it.
        (*loop_).data.sweep_next_tick_ns = now + LIBUS_TIMEOUT_GRANULARITY_NS;
        us_internal_timer_sweep(loop_);
    }
}

/// Emits the timeouts that are due: each group's tick advances, and a socket whose stamp it reaches times out.
unsafe fn us_internal_timer_sweep(loop_: *mut us_loop_t) {
    // SAFETY: the caller passes a live loop; handlers may unlink sockets and groups, which the iterators follow.
    unsafe {
        let data = &raw mut (*loop_).data;
        (*data).iterator = (*data).head;
        while !(*data).iterator.is_null() {
            let group = (*data).iterator;
            (*group).global_tick = (*group).global_tick.wrapping_add(1);
            let short_ticks = ((*group).global_tick % 240) as u8;
            let long_ticks = (((*group).global_tick / 15) % 240) as u8;
            (*group).timestamp = short_ticks;
            (*group).long_timestamp = long_ticks;

            let mut s = (*group).head_sockets;
            let mut group_survived = true;
            while !s.is_null() {
                if short_ticks != (*s).timeout && long_ticks != (*s).long_timeout {
                    s = (*s).next;
                    continue;
                }
                (*group).iterator = s;
                if short_ticks == (*s).timeout {
                    (*s).timeout = 255;
                    us_dispatch_timeout(s);
                }
                if (*data).iterator != group {
                    group_survived = false;
                    break;
                }
                if (*group).iterator == s && long_ticks == (*s).long_timeout {
                    (*s).long_timeout = 255;
                    us_dispatch_long_timeout(s);
                }
                if (*data).iterator != group {
                    group_survived = false;
                    break;
                }
                s = if s == (*group).iterator { (*s).next } else { (*group).iterator };
            }
            if group_survived {
                (*group).iterator = ptr::null_mut();
                (*data).iterator = (*group).next;
            }
        }
    }
}

// ── pre, post, and freeing what closed ─────────────────────────────────────

unsafe fn us_internal_handle_dns_results(loop_: *mut us_loop_t) -> bool {
    // SAFETY: the caller passes a live loop; the list is shared with resolver threads under the loop's mutex.
    unsafe {
        let mutex = &raw mut (*loop_).data.mutex;
        Bun__lock(mutex);
        let mut c = (*loop_).data.dns_ready_head;
        (*loop_).data.dns_ready_head = ptr::null_mut();
        Bun__unlock(mutex);
        let any = !c.is_null();
        while !c.is_null() {
            let next = (*c).next;
            crate::context::us_internal_socket_after_resolve(c);
            c = next;
        }
        any
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn us_internal_free_closed_sockets(loop_: *mut us_loop_t) {
    // SAFETY: the caller passes a live loop; what is on its closed lists is freed nowhere else.
    unsafe {
        let mut s = (*loop_).data.closed_head;
        while !s.is_null() {
            let next = (*s).next;
            let size = (*s).alloc_size;
            us_poll_free(s.cast(), size, loop_);
            s = next;
        }
        (*loop_).data.closed_head = ptr::null_mut();

        let mut c = (*loop_).data.closed_connecting_head;
        while !c.is_null() {
            let next = (*c).next;
            crate::context::us_connecting_socket_dealloc(c);
            c = next;
        }
        (*loop_).data.closed_connecting_head = ptr::null_mut();
    }
}

unsafe fn us_internal_loop_pre(loop_: *mut us_loop_t) {
    // SAFETY: the caller passes a live loop.
    unsafe {
        (*loop_).data.iteration_nr = (*loop_).data.iteration_nr.wrapping_add(1);
        us_internal_handle_dns_results(loop_);
        if let Some(pre) = (*loop_).data.pre_cb {
            pre(loop_);
        }
    }
}

unsafe fn us_internal_loop_post(loop_: *mut us_loop_t) {
    // SAFETY: the caller passes a live loop.
    unsafe {
        us_internal_handle_dns_results(loop_);
        // A nested tick must not free what the outer dispatch may still read.
        if (*loop_).data.tick_depth <= 1 {
            us_internal_free_closed_sockets(loop_);
        }
        if let Some(post) = (*loop_).data.post_cb {
            post(loop_);
        }
    }
}

// ── waiting and dispatching ────────────────────────────────────────────────

/// The poll's event, decoded by the backend: loop.c's `us_internal_dispatch_ready_poll`.
unsafe fn us_internal_dispatch_ready_poll(p: *mut us_poll_t, error: c_int, eof: c_int, events: c_int) {
    // SAFETY: the caller passes a live poll of this crate.
    unsafe {
        match us_internal_poll_type(p) {
            POLL_TYPE_CALLBACK => {
                let a = p.cast::<us_internal_async>();
                if let Some(cb) = (*a).cb {
                    cb((*a).loop_);
                }
            }
            POLL_TYPE_SEMI_SOCKET => {
                if us_poll_events(p) & LIBUS_SOCKET_WRITABLE != 0 {
                    let s = p.cast::<us_socket_t>();
                    let mut connect_error = 0;
                    if error != 0 || eof != 0 {
                        connect_error = crate::socket::us_socket_get_error(s);
                        if connect_error == 0 {
                            connect_error = libc::ECONNRESET;
                        }
                    }
                    crate::context::us_internal_socket_after_open(s, connect_error);
                } else {
                    crate::listen::on_readable(p.cast());
                }
            }
            _ => crate::socket::dispatch_socket(p.cast(), error, eof, events),
        }
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn us_loop_run_bun_tick(loop_: *mut us_loop_t, timeout: *const libc::timespec, _now_ns: u64) {
    // SAFETY: the caller passes its thread's live loop, and null or a timespec.
    unsafe {
        if (*loop_).num_polls == 0 {
            return;
        }
        (*loop_).data.tick_depth += 1;
        us_internal_loop_pre(loop_);

        let mut timeout = timeout.as_ref().copied();
        let sweep_ns = us_internal_sweep_timeout_ns(loop_);
        if sweep_ns >= 0 {
            let sweep = libc::timespec { tv_sec: (sweep_ns / 1_000_000_000) as _, tv_nsec: (sweep_ns % 1_000_000_000) as _ };
            let sooner = timeout.is_none_or(|t| (t.tv_sec, t.tv_nsec) > (sweep.tv_sec, sweep.tv_nsec));
            if sooner {
                timeout = Some(sweep);
            }
        }
        (*loop_).pending_wakeups.swap(0, Ordering::Acquire);

        (*loop_).num_ready_polls = backend::wait(loop_, timeout);
        (*loop_).current_ready_poll = 0;
        backend::dispatch_ready_polls(loop_);
        // A full batch may have left more ready: take them without waiting.
        let mut drain_count = 48;
        while (*loop_).num_ready_polls == LIBUS_MAX_READY_POLLS as c_int && drain_count > 1 && (*loop_).num_polls > 0 {
            drain_count -= 1;
            (*loop_).num_ready_polls = backend::wait(loop_, Some(libc::timespec { tv_sec: 0, tv_nsec: 0 }));
            backend::dispatch_ready_polls(loop_);
        }
        us_internal_sweep_if_due(loop_);

        us_internal_loop_post(loop_);
        (*loop_).data.tick_depth -= 1;
    }
}

/// `us_loop_run`: ticks until nothing keeps the loop alive.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn us_loop_run(loop_: *mut us_loop_t) {
    // SAFETY: the caller passes its thread's live loop.
    unsafe {
        while (*loop_).num_polls > 0 {
            us_loop_run_bun_tick(loop_, ptr::null(), 0);
        }
    }
}

pub(crate) unsafe fn loop_link_group(loop_: *mut us_loop_t, group: *mut us_socket_group_t) {
    // SAFETY: the caller passes a live loop and an unlinked group.
    unsafe {
        (*group).next = (*loop_).data.head;
        (*group).prev = ptr::null_mut();
        if !(*loop_).data.head.is_null() {
            (*(*loop_).data.head).prev = group;
        }
        (*loop_).data.head = group;
    }
}

pub(crate) unsafe fn loop_unlink_group(loop_: *mut us_loop_t, group: *mut us_socket_group_t) {
    // SAFETY: the caller passes a live loop and a group linked into it.
    unsafe {
        if group == (*loop_).data.iterator {
            (*loop_).data.iterator = (*group).next;
        }
        if (*loop_).data.head == group {
            (*loop_).data.head = (*group).next;
            if !(*loop_).data.head.is_null() {
                (*(*loop_).data.head).prev = ptr::null_mut();
            }
        } else {
            (*(*group).prev).next = (*group).next;
            if !(*group).next.is_null() {
                (*(*group).next).prev = (*group).prev;
            }
        }
    }
}

/// Closes every socket of every group on the loop; 1 if there was any.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn us_loop_close_all_groups(loop_: *mut us_loop_t) -> c_int {
    // SAFETY: the caller passes a live loop; closing can unlink groups, so `next` is re-read when it was.
    unsafe {
        let mut g = (*loop_).data.head;
        let mut any = 0;
        while !g.is_null() {
            let mut next = (*g).next;
            if !(*g).head_sockets.is_null() || !(*g).head_connecting_sockets.is_null() {
                crate::context::group_close_all(g, false);
                any = 1;
            }
            if !next.is_null() && (*next).linked == 0 {
                next = (*loop_).data.head;
            }
            g = next;
        }
        any
    }
}
