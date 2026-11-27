//! Sockets: `src/runtime/socket` on `bun_js`, over uSockets in Rust (`bun_usockets_rs`).

pub mod uws_dispatch;

/// bun_io's polls, whose pointers are tagged in the loop's epoll set (src/io/posix_event_loop.rs). A build without
/// bun_io has none; bun_io's definition wins where it is linked.
///
/// # Safety
/// uSockets passes its loop and a tagged pointer of the ready set.
#[unsafe(no_mangle)]
#[linkage = "weak"]
pub unsafe extern "C" fn Bun__internal_dispatch_ready_poll(_loop: *mut core::ffi::c_void, _poll: *mut core::ffi::c_void) {}
