//! Bun-specific JS-visible APIs: `src/runtime/api` on `bun_js`.

/// `src/runtime/api/bun`: `Bun.spawn`, `Bun.spawnSync` and what they return.
pub mod bun {
    pub mod js_bun_spawn_bindings;
    pub mod resource_usage;
    pub mod subprocess;
}
pub mod bun_object;
pub mod glob;
/// The `rustPath` of js_bun_spawn_bindings.bind.ts, as JavaScriptCore's api.rs re-exports it.
pub use bun::js_bun_spawn_bindings;
pub(crate) mod unsafe_object;
