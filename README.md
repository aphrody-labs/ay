# ay

`ay` is a Rust workspace combining upstream crates from `vendor/`:

- `vendor/zed`: GPUI, editor, rope, sum_tree, LSP, terminal, agent
- `vendor/gpui-kit`: GPUI components and `gpui-fast`
- `vendor/bun`: non-JSC Rust crates
- `vendor/blitz` and `vendor/dioxus`: HTML/CSS rendering and UI packages
- `vendor/cloudflare-os`, `vendor/kitesurf`, `vendor/workers-rs`, `vendor/pingora`: edge and proxy crates

### Building

```sh
git clone --recurse-submodules --shallow-submodules https://github.com/aphrody-labs/ay.git
cd ay
git config core.hooksPath .githooks
cargo test --workspace
```

### Licensing

First-party code is dual-licensed under MIT and Apache-2.0. Submodules under `vendor/` keep their respective upstream licenses.
