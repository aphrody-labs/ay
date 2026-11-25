//! Zero-alloc scoped path builder (`push_scoped` RAII truncation guard) + CWE-22 jail
//! (`push_inside` / `climbs_out`), modeled on `oven-sh/bun`'s `bun_paths` (36 dependents).

use crate::error::{CoreError, CoreResult};

/// Reusable mutable UTF-8 path buffer eliminating per-entry `PathBuf` allocations in walks.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AphrodyPath {
    buf: String,
}

/// RAII guard returned by [`AphrodyPath::push_scoped`] that truncates the buffer back on `Drop`.
pub struct ScopedPathGuard<'a> {
    path: &'a mut AphrodyPath,
    saved_len: usize,
}

impl Drop for ScopedPathGuard<'_> {
    #[inline]
    fn drop(&mut self) {
        self.path.buf.truncate(self.saved_len);
    }
}

impl ScopedPathGuard<'_> {
    #[inline]
    #[must_use]
    pub fn as_str(&self) -> &str {
        self.path.as_str()
    }

    #[inline]
    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        self.path.as_bytes()
    }
}

impl AphrodyPath {
    #[inline]
    #[must_use]
    pub fn from_root(root: &str) -> Self {
        let mut buf = String::with_capacity(root.len().max(128));
        buf.push_str(root.trim_end_matches(['/', '\\']));
        Self { buf }
    }

    #[inline]
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.buf
    }

    #[inline]
    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        self.buf.as_bytes()
    }

    #[inline]
    #[must_use]
    pub const fn len(&self) -> usize {
        self.buf.len()
    }

    #[inline]
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.buf.is_empty()
    }

    /// Push a segment onto this path buffer and automatically truncate back when the guard drops.
    #[inline]
    pub fn push_scoped(&mut self, segment: &str) -> ScopedPathGuard<'_> {
        let saved_len = self.buf.len();
        let trimmed = segment.trim_matches(['/', '\\']);
        if !trimmed.is_empty() && trimmed != "." {
            if !self.buf.is_empty() && !self.buf.ends_with('/') {
                self.buf.push('/');
            }
            self.buf.push_str(trimmed);
        }
        ScopedPathGuard {
            path: self,
            saved_len,
        }
    }

    /// Resolve an untrusted relative path inside `self`, rejecting NULs, absolute prefixes,
    /// Windows drive letters (`:`), and any `..` traversal escaping the base root (CWE-22).
    pub fn push_inside(&mut self, untrusted: &str) -> CoreResult<()> {
        if untrusted.contains('\0') {
            return Err(CoreError::invalid_arg("path", "embedded NUL byte rejected"));
        }
        if untrusted.starts_with('/') || untrusted.starts_with('\\') || untrusted.contains(':') {
            return Err(CoreError::invalid_arg(
                "path",
                "absolute or drive-qualified path rejected inside jail",
            ));
        }
        let base_len = self.buf.len();
        let mut depth: usize = 0;

        for seg in untrusted.split(['/', '\\']) {
            if seg.is_empty() || seg == "." {
                continue;
            }
            if seg == ".." {
                if depth == 0 {
                    self.buf.truncate(base_len);
                    return Err(CoreError::invalid_arg(
                        "path",
                        "directory traversal escapes root jail",
                    ));
                }
                if let Some(slash) = self.buf.rfind('/')
                    && slash >= base_len
                {
                    self.buf.truncate(slash);
                }
                depth -= 1;
                continue;
            }
            if !self.buf.is_empty() && !self.buf.ends_with('/') {
                self.buf.push('/');
            }
            self.buf.push_str(seg);
            depth += 1;
        }
        Ok(())
    }

    /// Return `true` if `rel` attempts to climb above depth 0 or inject absolute/NUL/drive syntax.
    #[inline]
    #[must_use]
    pub fn climbs_out(rel: &str) -> bool {
        if rel.contains('\0') || rel.starts_with('/') || rel.starts_with('\\') || rel.contains(':')
        {
            return true;
        }
        let mut depth: isize = 0;
        for seg in rel.split(['/', '\\']) {
            if seg.is_empty() || seg == "." {
                continue;
            }
            if seg == ".." {
                depth -= 1;
                if depth < 0 {
                    return true;
                }
            } else {
                depth += 1;
            }
        }
        false
    }
}

/// Convert a native path to portable `/` form once at cross-platform serialization boundaries
/// (manifests, lockfiles, bundles, hashes, URLs), matching `oven-sh/bun`'s `bun_paths::to_portable`.
#[must_use]
pub fn to_portable(path: &str) -> String {
    if !path.contains('\\') {
        return path.to_owned();
    }
    path.replace('\\', "/")
}

/// Convert portable text read from a manifest/lockfile where `\a` means relative `./a`
/// (never rooted), matching `bun_paths::from_portable`.
#[must_use]
pub fn from_portable(text: &str) -> String {
    let normalized = text.replace('\\', "/");
    if let Some(rest) = normalized.strip_prefix('/') {
        format!("./{rest}")
    } else {
        normalized
    }
}

/// Convert portable text where leading `\` or `/` stays rooted (`/a`), matching
/// `bun_paths::portable::from_portable_rooted` (`tsconfig.json`, URLs).
#[must_use]
pub fn from_portable_rooted(text: &str) -> String {
    text.replace('\\', "/")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn portable_path_boundaries_match_bun_rules() {
        assert_eq!(
            to_portable("crates\\ay_core\\src\\pod.rs"),
            "crates/ay_core/src/pod.rs"
        );
        assert_eq!(from_portable("\\bin\\ay"), "./bin/ay");
        assert_eq!(from_portable("apps\\app\\cli.ts"), "apps/app/cli.ts");
        assert_eq!(from_portable_rooted("\\src\\index.ts"), "/src/index.ts");
    }

    #[test]
    fn push_scoped_truncates_cleanly_without_realloc() {
        let mut p = AphrodyPath::from_root("/srv/aphrody");
        let cap_before = p.buf.capacity();
        {
            let g = p.push_scoped("crates/core");
            assert_eq!(g.as_str(), "/srv/aphrody/crates/core");
        }
        assert_eq!(p.as_str(), "/srv/aphrody");
        assert_eq!(p.buf.capacity(), cap_before);
    }

    #[test]
    fn push_inside_blocks_cwe22_escape_and_allows_safe_subpaths() {
        let mut p = AphrodyPath::from_root("/srv/root");
        p.push_inside("a/b/../c/./file.wasm").expect("safe subpath");
        assert_eq!(p.as_str(), "/srv/root/a/c/file.wasm");

        let mut jail = AphrodyPath::from_root("/srv/root");
        assert!(jail.push_inside("../../etc/passwd").is_err());
        assert_eq!(jail.as_str(), "/srv/root");
        assert!(jail.push_inside("C:\\Windows\\System32").is_err());
        assert!(jail.push_inside("bad\0name").is_err());
        assert!(AphrodyPath::climbs_out("a/../../escape"));
        assert!(!AphrodyPath::climbs_out("a/b/../c"));
    }
}
