# ay

First-party Rust workspace (`0 vendor/`, `0 third_party/`) fusing Zed (`rope`, `sum_tree`, `gpui_util`), Bun non-JSC (`bun_wyhash`, `bun_hpack`), Blitz (`blitz-traits`), Design (`design-engine`), Craft (`aphrody-craft`), `ay_macros` (`tw!`), `ay_ui` (AG-UI materializer, SSR, immediate-mode, OpenPets Rust), `ay_net` (`axum 0.8.9`, `lol_html 3.0.3`, `pingora-ketama 0.9.1`, `TinyUFO 0.9.1` via `crates.io`), and the `ay` CLI (`ay_cli`).

## Build & Verify

```sh
cargo fmt --all -- --check
cargo test --workspace
cargo clippy --workspace --all-targets --no-deps -- -D warnings
cargo run -p ay_cli -- version
cargo run -p ay_cli -- create demo
cargo run -p ay_cli -- dev --mobile --url http://127.0.0.1:3000/live
cargo run -p ay_cli -- pets
```

## License

First-party crates are dual-licensed under `MIT OR Apache-2.0`. Absorbed upstream crates (`rope`) retain their declared SPDX license in their crate `Cargo.toml`.
