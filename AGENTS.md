# ay — First-Party Engine & Framework Workspace

## Live Discovery & Native-First Contract

- **100% First-Party (`0 vendor/`, `0 third_party/`)**: Fused GPUI + Bun (non-JSC) + Blitz + Zed core + Design + Craft + Axum/Cloudflare (`crates.io` only) under `crates/`.
- **Zero Pre-Training Memory**: Query `aphrody-docs` MCP (`docs.rs`, `crates.io`, `npm`) and read crate sources/manifests before writing code. Never duplicate upstream capabilities.
- **Native-First**: Write directly in Full GPUI, Full WGPU (`30.0.2`), Full Bun (`bun_aot` / non-JSC), and Full Axum (`0.8.9` + `worker 0.9.0` + `utoipa 6.0.0`). Zero FFI/IPC/WebSocket hops between Rust crates compiled together.
- **Windows = Linux**: Never exclude a feature on Windows — port via `unixwin` or execute transparently via `kiri-whpx`.
- **Fail-Fast & Concise**: Clear structured errors (`thiserror::Error` + `Result`), zero `.unwrap()` on fallible OS/user paths, `try_reserve` for untrusted sizes, zero silent fallbacks, thin CLIs with compact JSON/Markdown agent outputs, `[lib] path = "src/<crate>.rs"`, no `mod.rs`.
- **Safe Bytes & Paths (Bun + `google/zerocopy`)**: Enforce `#![forbid(unsafe_code)]`; never use `transmute` or `from_raw_parts` on bytes. Use `aphrody_core::pod` (`bytes_of`, `slice_as_bytes`, `read_prefix`, `vec_from_bytes`, `unaligned_slice`) with explicit `#[repr(C)]` and `Unaligned` for wire/JS buffers; convert paths at serialization boundaries with `to_portable` / `from_portable` and guard untrusted input with `AphrodyPath::push_inside` / `climbs_out`.
- **Real Verification**: `cargo fmt --all -- --check`, `cargo test --workspace`, `cargo clippy --workspace --all-targets --no-deps -- -D warnings`, `cargo shear`, `ay gate --strict` (`TRUSTED.toml` + `acceptance.lock`), plus real binary/browser/`agent-desktop` execution proofs.
- **Autonomous NHITL**: Decide and deliver end-to-end without trivial questions; coordinate with peer agents via `/ws/agents`, ACP, A2A, and Code Mode MCP.
