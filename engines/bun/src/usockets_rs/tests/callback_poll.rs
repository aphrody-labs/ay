//! A callback poll on a descriptor of the caller's: a tick waits for it, runs its callback while it is readable,
//! and the loop forgets it once it is freed.
#![cfg(any(target_os = "linux", target_os = "android"))]

use core::ffi::{c_char, c_int, c_void};
use core::sync::atomic::{AtomicUsize, Ordering};

use bun_usockets_rs::{
    us_bun_verify_error_t, us_internal_callback_poll_free, us_internal_create_callback_poll, us_loop_t,
};

// What the binary that links uSockets defines (src/runtime/socket/uws_dispatch.rs, bun_io): this test makes no
// sockets, so none of them runs.
macro_rules! never {
    ($($name:ident($($arg:ty),*) -> $ret:ty;)*) => {$(
        #[unsafe(no_mangle)]
        extern "C" fn $name($(_: $arg),*) -> $ret {
            unreachable!(concat!(stringify!($name), " without a socket"))
        }
    )*};
}
never! {
    us_dispatch_open(*mut c_void, c_int, *mut c_char, c_int) -> *mut c_void;
    us_dispatch_data(*mut c_void, *mut c_char, c_int) -> *mut c_void;
    us_dispatch_fd(*mut c_void, c_int) -> *mut c_void;
    us_dispatch_socket_server_name(*mut c_void, *const c_char, *mut c_int) -> *mut c_void;
    us_dispatch_writable(*mut c_void) -> *mut c_void;
    us_dispatch_close(*mut c_void, c_int, *mut c_void) -> *mut c_void;
    us_dispatch_timeout(*mut c_void) -> *mut c_void;
    us_dispatch_long_timeout(*mut c_void) -> *mut c_void;
    us_dispatch_end(*mut c_void) -> *mut c_void;
    us_dispatch_connect_error(*mut c_void, c_int) -> *mut c_void;
    us_dispatch_connecting_error(*mut c_void, c_int) -> *mut c_void;
    Bun__internal_dispatch_ready_poll(*mut c_void, *mut c_void) -> ();
}

#[unsafe(no_mangle)]
extern "C" fn us_dispatch_handshake(_: *mut c_void, _: c_int, _: us_bun_verify_error_t) {
    unreachable!("us_dispatch_handshake without a socket")
}


unsafe extern "C" {
    #[link_name = "uws_get_loop"]
    fn uws_get_loop_raw() -> *mut c_void;
    #[link_name = "us_loop_run_bun_tick"]
    fn us_loop_run_bun_tick_raw(loop_: *mut c_void, timeout: *const libc::timespec, now_ns: u64);
}

fn uws_get_loop() -> *mut us_loop_t {
    // SAFETY: no arguments; the thread's loop.
    unsafe { uws_get_loop_raw().cast() }
}

static CALLS: AtomicUsize = AtomicUsize::new(0);

unsafe extern "C" fn count(_: *mut us_loop_t) {
    CALLS.fetch_add(1, Ordering::SeqCst);
}

fn tick(loop_: *mut us_loop_t, ms: i64) {
    let timeout = libc::timespec { tv_sec: ms / 1000, tv_nsec: ms % 1000 * 1_000_000 };
    // SAFETY: this thread's loop.
    unsafe { us_loop_run_bun_tick_raw(loop_.cast(), &timeout, 0) };
}

#[test]
fn a_readable_descriptor_runs_its_callback() {
    let mut fds = [0; 2];
    // SAFETY: `fds` has room for both ends.
    assert_eq!(unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_NONBLOCK | libc::O_CLOEXEC) }, 0);
    let loop_ = uws_get_loop();
    // SAFETY: this thread's loop, and a descriptor that outlives the poll.
    let poll = unsafe { us_internal_create_callback_poll(loop_, fds[0], count) };
    assert!(!poll.is_null());

    // Nothing to read: the tick waits out its timeout.
    let start = std::time::Instant::now();
    tick(loop_, 30);
    assert!(start.elapsed() >= std::time::Duration::from_millis(25));
    assert_eq!(CALLS.load(Ordering::SeqCst), 0);

    // SAFETY: the write end of the pipe.
    assert_eq!(unsafe { libc::write(fds[1], b"x".as_ptr().cast(), 1) }, 1);
    tick(loop_, 1000);
    assert_eq!(CALLS.load(Ordering::SeqCst), 1);
    // Level-triggered: still readable, so the next tick runs it again.
    tick(loop_, 1000);
    assert_eq!(CALLS.load(Ordering::SeqCst), 2);

    // SAFETY: the poll made above; then the descriptors, which the poll no longer uses.
    unsafe {
        us_internal_callback_poll_free(poll);
        libc::close(fds[0]);
        libc::close(fds[1]);
    }
    // No poll is left, so a tick returns at once instead of waiting.
    let start = std::time::Instant::now();
    tick(loop_, 1000);
    assert!(start.elapsed() < std::time::Duration::from_millis(500));
    assert_eq!(CALLS.load(Ordering::SeqCst), 2);
}
