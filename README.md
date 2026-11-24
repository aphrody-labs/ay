# ay

Rust workspace that links upstream crates from `vendor/zed`, `vendor/gpui-kit`, `vendor/bun`, `vendor/blitz`, `vendor/dioxus`, `vendor/cloudflare-os`, `vendor/kitesurf`, `vendor/workers-rs`, and `vendor/pingora` directly into native Rust builds.

### Developing

Clone with shallow submodules and enable the repository pre-commit hook:

```sh
git clone --recurse-submodules --shallow-submodules https://github.com/aphrody-labs/ay.git
cd ay
git config core.hooksPath .githooks
```

Run tests and lints:

```sh
cargo test --workspace
cargo clippy --workspace --all-targets --no-deps -- -D warnings
```

### Licensing

First-party code in this repository is dual-licensed under MIT and Apache-2.0. Submodules under `vendor/` retain their upstream licenses.
