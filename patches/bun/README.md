# Managed Upstream Bun Patches

Base upstream reference: `oven-sh/bun` (graph snapshot `6bd9289db`).

Instead of carrying 2.7M lines of forked `oven-sh/bun` C++/JSC/Rust source inside this repository, Aphrody keeps:
1. **Our own pure-Rust zero-JSC framework & runtime** in `crates/` (root `Cargo.toml`).
2. **Our 13 Bun plugins** in `packages/bun-plugin-*` (plus `packages/bun-types`).
3. **These minimal, reviewable patches** in `patches/bun/` to apply on top of a fresh `oven-sh/bun` checkout when building custom Bun binaries.

## Applying

```sh
git clone --depth 1 https://github.com/oven-sh/bun.git /tmp/oven-sh-bun
cd /tmp/oven-sh-bun
git apply C:/src/aphrody/patches/bun/0002-css-tailwind-theme.patch
git apply C:/src/aphrody/patches/bun/0003-shell-parser-uutils-builtins.patch
```

See `series` for the ordered patch list.
