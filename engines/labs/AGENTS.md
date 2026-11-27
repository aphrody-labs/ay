# labs

One Cargo workspace holding the forks and the crates Bun takes in (boa, blitz, stylo, kitesurf, rubash, parley,
vello_cpu, taffy, anyrender, ratatui-image), each a `git subtree`, so they can be purged, patched, deduplicated and
tuned together before moving into oven-sh/bun as first-party crates.

- `labs.toml` lists the trees and what is purged; `bun run sync` adds/pulls them, `bun run unify` regenerates the
  root `Cargo.toml` (never edit it by hand). A tree's own workspace tables live in `workspaces/<tree>.toml`.
  A pull is one patch commit (upstream's range since the `git-subtree-split` trailer), never a merge of upstream's
  history: GitHub refuses pushes naming LFS objects labs does not store (GH008).
- Dependencies: `bun run bump [cargo upgrade args]` raises requirements and writes the root's back into
  `workspaces/*.toml`; then `cargo update && bun run unify` (unify re-applies the `[lock]` pins). `bun run deps [label]`
  appends crate/package and duplicated-name counts to `build/deps.jsonl`. A version that adds a duplicate stays.
- labs' own TypeScript (`scripts/`): `bun run typecheck`, `bun run lint`, `bun run fmt`. Bun's `check`, `lint`
  and `fmt` run the package.json script of that name when there is one: pass `--no-script` (`check` is cargo).
- Lints: `bun run remote bun run clippy` (`cargo clippy --workspace --all-targets -- -D warnings`) and
  `cargo fmt --all --check` pass. labs.toml `[lints]` (every member) and `[trees.<tree>.lints]` are written into
  members' `[lints]` by unify: rubash and stylo, whose upstreams run no clippy, keep only `clippy::correctness`.
  Targets follow `purge`: no example or bench targets, and a test that names a dropped crate is left out, so
  `--all-targets` builds.
- Patches are commits on the subtree. kitesurf's vendor series (`kitesurf/scripts/patches/<crate>-<version>/`) are
  ported onto the in-tree crates: `bun run port kitesurf [crate…]` replays each series on its crates.io tarball into
  `refs/port/<series>`, then `git am -3 --directory=<crate path>` merges it for real (`git show refs/port/…` is the
  old patched source). usvg and jpeg-encoder have no in-tree crate and build unpatched.
- Builds run on n1 only: `bun run remote bun run check`, `bun run remote bun run bench`. wasm32 (kitesurf's target)
  builds without sccache: its dist workers have no wasm std (`RUSTC_WRAPPER= cargo check --target wasm32-unknown-unknown`).
- Native code (C, C++, asm) stays only where it is faster than the Rust equivalent or none exists; `deny.toml`
  lists what is banned. Every change is measured: `bun run bench` appends to `build/bench.jsonl`.
