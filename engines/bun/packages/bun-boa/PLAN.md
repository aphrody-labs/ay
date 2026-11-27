# Bun without JavaScriptCore and without C++

`bun-boa` builds a `bun` binary from Bun's Rust crates in which every C/C++ dependency is replaced by Rust.
It merges Bun, Dioxus/Blitz and kitesurf without breaking any of them and without weighing on Bun:

- `packages/bun-boa/` is a standalone Cargo workspace (the Dioxus layout: a crate in `packages/`). Bun's root
  workspace and lockfile are untouched; Bun's `src/*` crates are path dependencies.
- Boa and Blitz are first-party: `src/boa` and `src/blitz` are `git subtree`s (history kept) of the forks'
  `bun/v0.22.0` (aphrody-dev/boa) and `bun/v0.3.0-beta.4` (aphrody-dev/blitz) branches: the release tag with
  kitesurf's patch series, then ours, as commits. Changes are commits there (`scripts/port-pr.ts` ports an
  upstream PR as one), published with `git subtree push`. Crates keep their crates.io names: Blitz's workspace
  and kitesurf-cdp depend on them by those names, and `[patch.crates-io]` points every such user at the
  in-tree copy.
- `patches/<crate>-<version>/` holds kitesurf's git-format-patch series against crates.io tarballs for the rest
  of the stack (Stylo, Taffy, Parley, vello_cpu, anyrender, usvg). `bun run vendor` materialises them into the
  gitignored `vendor/`, and `[patch.crates-io]` wires them in.
- Builds never run locally: `bun run build` / `bun run test` (`scripts/remote.ts`) give the build host (n1,
  `/srv/xbuild` XFS) its own clone of HEAD from GitHub, rsync only the local delta, run `scripts/setup.ts`
  (Bun's vendored Rust deps from `scripts/build/deps`, the patched crates, and the codegen commands from
  `build.ninja`), then cargo with sccache-dist across the cluster.

## Replacement map

| C/C++ today                                   | Rust replacement                                                      |
| --------------------------------------------- | --------------------------------------------------------------------- |
| WebKit JavaScriptCore                         | Boa (fork)                                                            |
| WebCore (DOM, HTML/CSS, WebView)              | Blitz (fork): blitz-dom/html, Stylo, Taffy, Parley, Vello / vello_cpu |
| WebCore streams, Blob, Response, events       | Rust on Boa in `src/boa`                                              |
| WTF strings, dtoa, `parseDouble`, URL         | Rust (`ryu-js`, `fast-float2`, `url`)                                 |
| ICU (bundled with WebKit)                     | ICU4X (already used by Boa)                                           |
| BoringSSL (TLS, WebCrypto, `node:crypto`)     | rustls + `rustls-rustcrypto`, RustCrypto crates                       |
| Highway SIMD kernels, simdutf                 | Rust (`std::simd`/scalar) in the native shim module                   |
| mimalloc                                      | Rust global allocator                                                 |
| uSockets / uWebSockets (`uws_sys`)            | Rust sockets on `mio`, `bun_http1`/`bun_hpack`/`bun_websocket`        |
| lsquic / ls-qpack (HTTP/3)                    | `quinn` + `h3`                                                        |
| SQLite (`bun:sqlite`)                         | Turso/Limbo (`turso_core`)                                            |
| TinyCC (`bun:ffi` `cc`)                       | Cranelift JIT                                                         |
| libuv (Windows event loop)                    | Rust IOCP (`mio`/`compio`)                                            |
| V8 C++ API shim for native addons             | Dropped. N-API (C ABI) gets implemented in Rust over Boa              |

Already Rust and reused as is: `js_parser`, `js_printer`, `ast`, `resolver`, `css`, `lint`, `format`,
`sourcemap`, `semver`, `glob`, `md`, `ini`, `dotenv`, `zstd`, lol-html, rust-argon2.

## Phases

0. **Infra**: Bun codegen without WebKit, Boa and Blitz in-tree, Rust native shims (start from
   `bun_sema_standalone::native`), and a test that the linked binary has no C++ in it.
1. **Core runtime**: Bun's transpiler, then Boa ES modules, an event loop (microtasks and timers),
   `console`, `process`, `Bun.file`/`Bun.write`/`Bun.env`/`Bun.version`, top-level await.
2. **CLI reuse**: `bun fmt`, `bun lint`, `bun run <script>`, `bun build` (decouple
   `bundler`→`event_loop`→`uws` from `bun_jsc_macros`/uWS), `bun install`/`add`/`remove` (the `install` feature:
   `bun_install` without `bun_jsc`, on `src/usockets_rs`, which is uSockets' loop and TCP and TLS clients in Rust
   (rustls with RustCrypto), and the ports in `src/install`: WTF strings, URL and dates, the digests,
   `posix_spawn_bun`. What is left of C/C++ on its path is listed in `src/install/native.rs`).
3. **Web APIs**: `fetch` (Rust HTTP plus rustls), `URL`, `TextEncoder`/`TextDecoder`, streams, `Blob`,
   WebCrypto, `Bun.serve`.
4. **Node compat**: run Bun's own `src/js/node/*` builtins on Boa by providing their `$` intrinsics in Rust.
5. **Blitz**: headless `Bun.WebView` on Blitz (vello_cpu screenshots), `HTMLRewriter` on lol-html. First
   piece: `cdp/` (`kitesurf-cdp serve --host 127.0.0.1 --port N`), a headless browser on kitesurf's Blitz
   stack and `blitz-vibey-script` (Boa DOM bindings, in `src/blitz`) behind the DevTools protocol, the
   `lightpanda serve` contract agent-browser launches. `bun run build:cdp`, `bun run test:cdp`.
6. **The rest**: `bun:sqlite` on Turso, `bun:ffi` on Cranelift, HTTP/3 on quinn, Windows I/O.
7. **Conformance**: run Bun's `test/js` suite with this binary and track the pass rate. Engine
   performance work happens in `src/boa`.
