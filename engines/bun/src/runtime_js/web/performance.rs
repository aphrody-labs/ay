//! The bodies of `Performance.bind.ts`: the process's time origin, as `VirtualMachine`'s `origin_timer` and
//! `origin_timestamp` are for `Bun__readOriginTimer` and `Bun__readOriginTimerStart`.

use std::sync::OnceLock;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

struct Origin {
    instant: Instant,
    unix_ms: f64,
}

static ORIGIN: OnceLock<Origin> = OnceLock::new();

fn origin() -> &'static Origin {
    ORIGIN.get_or_init(|| Origin {
        instant: Instant::now(),
        unix_ms: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0.0, |since| since.as_nanos() as f64 / 1_000_000.0),
    })
}

/// Fixes the time origin, if nothing read it yet: the embedder calls it when the process starts.
pub fn start() {
    origin();
}

/// Bun's `Performance::now()`: the nanoseconds of the origin timer, in milliseconds.
pub(crate) fn now() -> f64 {
    origin().instant.elapsed().as_nanos() as f64 / 1_000_000.0
}

/// The nanoseconds of the origin timer, as `Bun__readHRTime` reads them.
pub(crate) fn elapsed_ns() -> u64 {
    origin().instant.elapsed().as_nanos() as u64
}

pub(crate) fn time_origin() -> f64 {
    origin().unix_ms
}
