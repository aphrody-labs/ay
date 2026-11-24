# Engineering guidelines

## Architecture

* Link upstream crates directly from `vendor/` via Cargo path dependencies. Do not duplicate upstream code or wrap it in placeholder structs.
* Keep Rust components linked in one native binary. Do not add FFI or WebSocket bridges between Rust crates.
* Specify `[lib] path = "src/<crate>.rs"` in `Cargo.toml` instead of default `lib.rs` and avoid `mod.rs` files.

## Style and hygiene

* Write comments only to explain why a non-obvious constraint exists, never to summarize code.
* Use `<subsystem>: <concise change>` commit messages (Zed/Bun style), authored as `aphrody-dev <contact@aphrody.com>`.
* Keep Markdown prose plain and direct. Avoid marketing adjectives, bold-label bullet lists, and private data.

## Verification

```sh
cargo fmt -p ay -- --check
cargo test --workspace
cargo clippy --workspace --all-targets --no-deps -- -D warnings
```
