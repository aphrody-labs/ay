//! `__bun_run_io` (src/runtime/dispatch.rs): what bun_io calls when one of its polls fires. The owners of the polls
//! are in crates this one does not link (`bun install`'s subprocesses), so the arm comes from [`register_run_io`].

use std::sync::OnceLock;

use bun_io::{IoEvent, Owner};

type RunIo = unsafe fn(Owner, IoEvent);

static RUN_IO: OnceLock<RunIo> = OnceLock::new();

/// The arm of the owners that another crate makes. Once, before a poll of theirs exists.
pub fn register_run_io(run: RunIo) {
    let _ = RUN_IO.set(run);
}

/// # Safety
/// `owner.ptr()` is the live value that the registered arm casts it to.
#[unsafe(no_mangle)]
unsafe fn __bun_run_io(owner: Owner, event: IoEvent) {
    let run = RUN_IO
        .get()
        .unwrap_or_else(|| panic!("a poll of PollTag {} fired, and nothing in this build owns it", owner.tag() as u8));
    // SAFETY: the contract of this function is the registered arm's.
    unsafe { run(owner, event) }
}
