//! `aphrody-core` — Bun-inspired pure-Rust systems foundation (0 JSC, 0 WebKit, 0 V8).
//!
//! Modeled directly on upstream `oven-sh/bun`'s pure-Rust hub crates (`bun_core`, `bun_paths`,
//! `bun_wyhash`, `bun_histogram`, `bun_dotenv`, `bun_glob`, `bun_semver`):
//! - Zero `Box<dyn>` on hot paths (typed `SlotId(u32)` + generational [`Arena`] + stack [`BoundedArray`]).
//! - Zero-allocation `&[u8]` scanning/splitting ([`bytes`]).
//! - Compile-time + runtime byte-identical 64-bit hashing ([`wyhash`]).
//! - Zero-alloc RAII scoped paths + CWE-22 jail ([`path`]).
//! - Real logarithmic latency percentiles ([`histogram`]).
//! - Pure-Rust `.env`, Glob, and SemVer 2.0 engines ([`dotenv`], [`glob`], [`semver`]).
//! - Static [`CoreError`] enum with `#[cold]` constructors & formatting ([`error`]).
//! - Scoped `APHRODY_DEBUG_*` logger ([`log`]) & 64-bit [`CapabilityFlags`] ([`caps`]).

#![forbid(unsafe_code)]

pub mod arena;
pub mod bounded;
pub mod bytes;
pub mod caps;
pub mod dotenv;
pub mod error;
pub mod glob;
pub mod histogram;
pub mod log;
pub mod path;
pub mod pod;
pub mod semver;
pub mod wyhash;

pub use arena::{Arena, SlotId};
pub use bounded::BoundedArray;
pub use bytes::{decode_utf8, pinned_kernel_tag};
pub use caps::CapabilityFlags;
pub use dotenv::DotEnvLoader;
pub use error::{CoreError, CoreResult};
pub use glob::GlobPattern;
pub use histogram::HdrHistogram;
pub use log::ScopedLogger;
pub use path::{AphrodyPath, ScopedPathGuard, from_portable, from_portable_rooted, to_portable};
pub use pod::{
    bytes_of, read_prefix, slice_as_bytes, try_cast_slice, unaligned_slice, vec_from_bytes,
};
pub use semver::{Version, VersionReq};
pub use wyhash::{wyhash as hash64, wyhash_const as hash64_const};
