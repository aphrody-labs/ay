//! Tuned Rust-lang depot tools, universal 10-ecosystem package IR & zero-copy `depot.lock`,
//! Nix+CachyOS conflict auditor, `Cargo.lock` zero-JS-engine audit, and docs-provider catalog (`#![forbid(unsafe_code)]`).

pub mod ir;
pub mod lockfile;
pub mod nix_cachyos;

pub use ir::{ActionPlane, BackendProbe, DepotError, Ecosystem, Purl, probe_all_backends};
pub use lockfile::{
    DEPOT_LOCK_MAGIC, DEPOT_LOCK_VERSION, DepotLockEntry, DepotLockHeader, LockRecord,
    decode_depot_lock, encode_depot_lock,
};
pub use nix_cachyos::{ConflictFinding, NixCachyosAudit, audit_nix_cachyos};

use std::collections::{BTreeMap, BTreeSet};

use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct DepotRepoSpec {
    pub name: &'static str,
    pub url: &'static str,
    pub role: &'static str,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct DocsSourceSpec {
    pub id: &'static str,
    pub repo_or_url: &'static str,
    pub prefix: &'static str,
    pub license: &'static str,
}

pub const RUST_DEPOT_REPOS: &[DepotRepoSpec] = &[
    DepotRepoSpec {
        name: "cargo",
        url: "https://github.com/rust-lang/cargo",
        role: "package manager & resolver v3",
    },
    DepotRepoSpec {
        name: "rustup",
        url: "https://github.com/rust-lang/rustup",
        role: "toolchain multiplexer",
    },
    DepotRepoSpec {
        name: "rust-analyzer",
        url: "https://github.com/rust-lang/rust-analyzer",
        role: "LSP & incremental HIR/MIR",
    },
    DepotRepoSpec {
        name: "crates.io",
        url: "https://github.com/rust-lang/crates.io",
        role: "sparse registry & index",
    },
    DepotRepoSpec {
        name: "rustc-perf",
        url: "https://github.com/rust-lang/rustc-perf",
        role: "compiler & build-time benchmark suite",
    },
    DepotRepoSpec {
        name: "docs.rs",
        url: "https://github.com/rust-lang/docs.rs",
        role: "rustdoc builder & hosting",
    },
    DepotRepoSpec {
        name: "reference",
        url: "https://github.com/rust-lang/reference",
        role: "Rust Edition 2024 language reference",
    },
    DepotRepoSpec {
        name: "rust-repos",
        url: "https://github.com/rust-lang/rust-repos",
        role: "official rust-lang org inventory",
    },
    DepotRepoSpec {
        name: "rustc-pr-tracking",
        url: "https://github.com/rust-lang/rustc-pr-tracking",
        role: "compiler PR & release tracking",
    },
    DepotRepoSpec {
        name: "gcc",
        url: "https://github.com/rust-lang/gcc",
        role: "libgccjit / rustc_codegen_gcc backend",
    },
    DepotRepoSpec {
        name: "zerocopy",
        url: "https://github.com/google/zerocopy",
        role: "sound zero-copy transmutes & wire layouts",
    },
    DepotRepoSpec {
        name: "ninja-to-soong",
        url: "https://github.com/google/ninja-to-soong",
        role: "Ninja to AOSP Soong build converter",
    },
    DepotRepoSpec {
        name: "bun-aot-mdx",
        url: "https://github.com/oven-sh/bun",
        role: "src/aot pure-Rust AOT + bun-build-mdx-rs",
    },
    DepotRepoSpec {
        name: "depot_tools",
        url: "https://chromium.googlesource.com/chromium/tools/depot_tools.git",
        role: "Chromium gclient/CIPD/Siso reference",
    },
];

pub const DOCS_SOURCES: &[DocsSourceSpec] = &[
    DocsSourceSpec {
        id: "cloudflare-docs",
        repo_or_url: "cloudflare/cloudflare-docs",
        prefix: "src/content/docs/",
        license: "CC-BY-4.0",
    },
    DocsSourceSpec {
        id: "cloudflare-nimbus",
        repo_or_url: "cloudflare/nimbus",
        prefix: "",
        license: "Apache-2.0",
    },
    DocsSourceSpec {
        id: "google-styleguide",
        repo_or_url: "google/styleguide",
        prefix: "",
        license: "CC-BY-3.0",
    },
    DocsSourceSpec {
        id: "bun-docs",
        repo_or_url: "oven-sh/bun",
        prefix: "docs/",
        license: "MIT",
    },
    DocsSourceSpec {
        id: "bun-types",
        repo_or_url: "oven-sh/bun",
        prefix: "packages/bun-types/",
        license: "MIT",
    },
    DocsSourceSpec {
        id: "bun-mdx-rs",
        repo_or_url: "oven-sh/bun",
        prefix: "packages/bun-build-mdx-rs/",
        license: "MIT",
    },
    DocsSourceSpec {
        id: "bun-aot",
        repo_or_url: "oven-sh/bun",
        prefix: "src/aot/",
        license: "MIT",
    },
    DocsSourceSpec {
        id: "microsoft-markitdown",
        repo_or_url: "microsoft/markitdown",
        prefix: "",
        license: "MIT",
    },
    DocsSourceSpec {
        id: "microsoft-tsdoc",
        repo_or_url: "microsoft/tsdoc",
        prefix: "",
        license: "MIT",
    },
    DocsSourceSpec {
        id: "microsoft-mcp",
        repo_or_url: "MicrosoftDocs/mcp",
        prefix: "",
        license: "MIT",
    },
    DocsSourceSpec {
        id: "mdn-content",
        repo_or_url: "mdn/content",
        prefix: "files/en-us/",
        license: "CC-BY-SA-2.5",
    },
    DocsSourceSpec {
        id: "w3c",
        repo_or_url: "https://www.w3.org/",
        prefix: "",
        license: "W3C",
    },
    DocsSourceSpec {
        id: "freedesktop",
        repo_or_url: "https://www.freedesktop.org/",
        prefix: "",
        license: "MIT",
    },
    DocsSourceSpec {
        id: "rust-doc",
        repo_or_url: "https://doc.rust-lang.org/",
        prefix: "",
        license: "MIT OR Apache-2.0",
    },
];

const FORBIDDEN_JS_ENGINES: &[&str] = &["v8", "rusty_v8", "rquickjs", "webkit2gtk", "skia-safe"];

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LockAudit {
    pub total_packages: usize,
    pub duplicate_crates: BTreeMap<String, Vec<String>>,
    pub forbidden_engines: Vec<String>,
}

/// Analyze a `Cargo.lock` string for duplicate crate versions and forbidden embedded C++ JS engines.
pub fn analyze_cargo_lock(lock_toml: &str) -> LockAudit {
    let mut versions: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let mut current_name: Option<String> = None;
    let mut total_packages = 0usize;

    for line in lock_toml.lines() {
        let trimmed = line.trim();
        if trimmed == "[[package]]" {
            total_packages += 1;
            current_name = None;
        } else if let Some(rest) = trimmed.strip_prefix("name = \"")
            && let Some(name) = rest.strip_suffix('"')
        {
            current_name = Some(name.to_owned());
        } else if let Some(rest) = trimmed.strip_prefix("version = \"")
            && let Some(ver) = rest.strip_suffix('"')
            && let Some(name) = current_name.as_deref()
        {
            versions
                .entry(name.to_owned())
                .or_default()
                .insert(ver.to_owned());
        }
    }

    let mut duplicate_crates = BTreeMap::new();
    let mut forbidden_engines = Vec::new();
    for (name, vers) in versions {
        if FORBIDDEN_JS_ENGINES.contains(&name.as_str()) {
            forbidden_engines.push(name.clone());
        }
        if vers.len() > 1 {
            duplicate_crates.insert(name, vers.into_iter().collect());
        }
    }

    LockAudit {
        total_packages,
        duplicate_crates,
        forbidden_engines,
    }
}

/// Generate a tuned `.cargo/config.toml` snippet using `lld`, `sccache`, and `target-cpu`.
pub fn tuned_cargo_config(target_cpu: &str) -> String {
    format!(
        r#"[build]
rustc-wrapper = "sccache"

[target.x86_64-unknown-linux-gnu]
rustflags = ["-C", "link-arg=-fuse-ld=lld", "-C", "target-cpu={target_cpu}"]

[profile.release]
lto = "thin"
codegen-units = 1
strip = "symbols"
"#
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalogs_are_complete() {
        assert_eq!(RUST_DEPOT_REPOS.len(), 14);
        assert_eq!(DOCS_SOURCES.len(), 14);
        assert!(RUST_DEPOT_REPOS.iter().any(|r| r.name == "cargo"));
        assert!(DOCS_SOURCES.iter().any(|s| s.id == "cloudflare-docs"));
    }

    #[test]
    fn lock_audit_detects_duplicates_and_forbidden_engines() {
        let sample = "[[package]]\nname = \"zerocopy\"\nversion = \"0.8.65\"\n[[package]]\nname = \"zerocopy\"\nversion = \"0.8.66\"\n[[package]]\nname = \"v8\"\nversion = \"130.0.0\"\n";
        let audit = analyze_cargo_lock(sample);
        assert_eq!(audit.total_packages, 3);
        assert_eq!(audit.duplicate_crates.len(), 1);
        assert_eq!(audit.forbidden_engines, vec!["v8"]);
    }
}
