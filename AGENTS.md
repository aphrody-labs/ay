# Workspace guide

`crates/ay` depends on upstream crates under `vendor/` via Cargo path dependencies (`Cargo.toml`).

### Build and test

```sh
cargo fmt -p ay -- --check
cargo test --workspace
cargo clippy --workspace --all-targets --no-deps -- -D warnings
```

### Conventions

* Use `[lib] path = "src/<crate>.rs"` in crate manifests and avoid `mod.rs`.
* Commit messages follow `<subsystem>: <concise change>`.
* Keep `vendor/` shallow; put local changes in first-party crates or patches.
