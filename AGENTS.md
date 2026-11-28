# ay — Pure-Rust Runtime & Edge Core Workspace

## Live Discovery & Native-First Contract

- **100% First-Party (`0 vendor/`, `0 third_party/`, `0 WebKit`, `0 JSC`, `0 UI bloat`)**: Pure-Rust runtime core (`ay`, `ay_core`, `ay_net`, `ay_cli`, `ay_depot`, `ay_bridge`, `bun_wyhash`, `bun_hpack`, `rope`, `sum_tree`, `gpui_util`, `ztracing`) under `crates/`. All UI/Design/Craft/GPUI/WGSL crates live in `aphrody-labs/aui`; Wasm/binary/Naga compilation lives in `aphrody-labs/awc`.
- **Zero Pre-Training Memory**: Query live crate manifests, `docs.rs`, and `crates.io` before writing code. Never duplicate upstream capabilities.
- **Native-First**: Write directly in pure Rust (`axum 0.8.9`, `lol_html 3.0.3`, `pingora-ketama`, `TinyUFO`, `bun_wyhash`, `bun_hpack`, `rope`). Zero FFI/IPC/WebSocket hops between Rust crates compiled together.
- **Windows = Linux**: Never exclude a feature on Windows — bridge transparently via `ay_bridge` / WHPX / WSL2.
- **Fail-Fast & Concise**: Clear structured errors (`thiserror::Error` + `Result`), `#![forbid(unsafe_code)]`, `unwrap_used = "deny"`, `try_reserve` for untrusted sizes, zero silent fallbacks, thin CLIs with compact JSON/Markdown agent outputs, `[lib] path = "src/<crate>.rs"`, no `mod.rs`.
- **Safe Bytes & Paths (Bun + `google/zerocopy`)**: Never use `transmute` or `from_raw_parts` on bytes. Use `aphrody_core::pod` (`bytes_of`, `slice_as_bytes`, `read_prefix`, `vec_from_bytes`, `unaligned_slice`) with explicit `#[repr(C)]` and `Unaligned` for wire/JS buffers; convert paths at serialization boundaries with `to_portable` / `from_portable` and guard untrusted input with `AphrodyPath::push_inside` / `climbs_out`.
- **Real Verification**: `cargo fmt --all -- --check`, `cargo clippy -p ay -p ay_bridge -p ay_cli -p ay_depot -p ay_net -p aphrody-core -p bun_wyhash -p bun_hpack --all-targets --no-deps -- -D warnings`, `cargo test` on all first-party crates, plus real `ay doctor` execution proofs.
- **Autonomous NHITL**: Decide and deliver end-to-end without trivial questions.
