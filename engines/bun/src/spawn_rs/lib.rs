//! What src/jsc/bindings/bun-spawn.cpp and c-bindings.cpp give `bun_spawn_sys` and `bun_spawn`, in Rust: the
//! `posix_spawn_bun` that `spawn_process_posix` calls, and the signal forwarding of `bun_spawn::process::sync`.
//! The symbols are `#[no_mangle]`: a crate that spawns names this one (`use bun_spawn_rs as _`) so they are linked.

#![cfg(any(target_os = "linux", target_os = "android"))]

mod posix_spawn;
mod signals;
