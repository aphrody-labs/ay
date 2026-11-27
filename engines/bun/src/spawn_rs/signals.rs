//! Forwarding signals to the child of a spawnSync (`bun_spawn::process::sync`): src/jsc/bindings/c-bindings.cpp's
//! `Bun__currentSyncPID`, `Bun__registerSignalsForForwarding`, `Bun__unregisterSignalsForForwarding` and
//! `Bun__sendPendingSignalIfNecessary`, in Rust.

use core::ffi::c_int;
use core::sync::atomic::{AtomicI32, AtomicI64, Ordering::SeqCst};
use std::sync::Mutex;

/// The handler can run on any thread while the spawning thread stores the pid.
#[unsafe(no_mangle)]
pub static Bun__currentSyncPID: AtomicI64 = AtomicI64::new(0);

static PENDING_SIGNAL_TO_SEND: AtomicI32 = AtomicI32::new(0);

/// npm's signal list minus SIGIOT/SIGPOLL (aliases of SIGABRT/SIGIO) and minus SIGSYS, which the kernel raises for
/// the process's own syscall. SIGPWR is left alone, as JavaScriptCore's GC needs it.
const SIGNALS: &[c_int] = &[
    libc::SIGABRT,
    libc::SIGALRM,
    libc::SIGHUP,
    libc::SIGINT,
    libc::SIGTERM,
    libc::SIGVTALRM,
    libc::SIGXCPU,
    libc::SIGXFSZ,
    libc::SIGUSR2,
    libc::SIGTRAP,
    libc::SIGQUIT,
    libc::SIGIO,
    #[cfg(any(target_os = "linux", target_os = "android"))]
    libc::SIGSTKFLT,
];

/// The depth of nested registrations, and the actions they replaced. Detached threads can run spawnSync, so
/// registering and unregistering can overlap.
struct Forwarding {
    depth: usize,
    previous: Vec<(c_int, libc::sigaction)>,
}

static FORWARDING: Mutex<Forwarding> = Mutex::new(Forwarding {
    depth: 0,
    previous: Vec::new(),
});

extern "C" fn forward(sig: c_int) {
    let mut pid = Bun__currentSyncPID.load(SeqCst);
    if pid == 0 {
        PENDING_SIGNAL_TO_SEND.store(sig, SeqCst);
        pid = Bun__currentSyncPID.load(SeqCst);
        if pid == 0 || PENDING_SIGNAL_TO_SEND.swap(0, SeqCst) == 0 {
            return;
        }
    }
    // SAFETY: kill(2) takes integers only; async-signal-safe.
    unsafe { libc::kill(pid as libc::pid_t, sig) };
}

/// The spawning thread stores the pid, then reads the pending signal. The handler stores the signal, then loads the
/// pid. At least one of them sees the other's store, and the swap ensures only one of them sends the signal.
#[unsafe(no_mangle)]
pub extern "C" fn Bun__sendPendingSignalIfNecessary() {
    let sig = PENDING_SIGNAL_TO_SEND.swap(0, SeqCst);
    let pid = Bun__currentSyncPID.load(SeqCst);
    if sig == 0 || pid == 0 {
        return;
    }
    // SAFETY: kill(2) takes integers only.
    unsafe { libc::kill(pid as libc::pid_t, sig) };
}

#[unsafe(no_mangle)]
pub extern "C" fn Bun__registerSignalsForForwarding() {
    let mut forwarding = FORWARDING.lock().unwrap_or_else(|e| e.into_inner());
    forwarding.depth += 1;
    if forwarding.depth != 1 {
        return;
    }
    PENDING_SIGNAL_TO_SEND.store(0, SeqCst);
    // SAFETY: an all-zero `sigaction` is valid: no flags, an empty mask.
    let mut action: libc::sigaction = unsafe { core::mem::zeroed() };
    // Not SA_RESETHAND: a nested runner sees the signal more than once and must not die on a repeat delivery.
    action.sa_sigaction = forward as extern "C" fn(c_int) as libc::sighandler_t;
    // SAFETY: `action.sa_mask` is a valid sigset_t to write.
    unsafe { libc::sigemptyset(&mut action.sa_mask) };
    forwarding.previous.clear();
    for &sig in SIGNALS {
        // SAFETY: as above.
        let mut previous: libc::sigaction = unsafe { core::mem::zeroed() };
        // SAFETY: both pointers are to live `sigaction`s.
        if unsafe { libc::sigaction(sig, &action, &mut previous) } == 0 {
            forwarding.previous.push((sig, previous));
        }
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn Bun__unregisterSignalsForForwarding() {
    Bun__currentSyncPID.store(0, SeqCst);
    let mut forwarding = FORWARDING.lock().unwrap_or_else(|e| e.into_inner());
    forwarding.depth -= 1;
    if forwarding.depth != 0 {
        return;
    }
    for (sig, previous) in forwarding.previous.drain(..) {
        // SAFETY: `previous` is what `sigaction` gave back for `sig`.
        unsafe { libc::sigaction(sig, &previous, core::ptr::null_mut()) };
    }
}
