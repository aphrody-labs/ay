//! Pure-Rust zero-regex Glob matcher (`*`, `**`, `?`, `[a-z]`, `{a,b}`),
//! modeled on `oven-sh/bun`'s `bun_glob` (14 dependents).

/// Compiled glob pattern supporting `*`, `**`, `?`, character classes `[...]`, and brace `{a,b}`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GlobPattern {
    raw: String,
    alternatives: Vec<String>,
}

impl GlobPattern {
    #[must_use]
    pub fn compile(pattern: &str) -> Self {
        let normalized = pattern.replace('\\', "/");
        let alternatives = expand_braces(&normalized);
        Self {
            raw: normalized,
            alternatives,
        }
    }

    #[inline]
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.raw
    }

    /// Match a POSIX or Windows path against this glob pattern.
    #[inline]
    #[must_use]
    pub fn matches(&self, path: &str) -> bool {
        let norm_owned;
        let target = if path.contains('\\') {
            norm_owned = path.replace('\\', "/");
            norm_owned.as_str()
        } else {
            path
        };
        let target_bytes = target.as_bytes();
        self.alternatives
            .iter()
            .any(|pat| match_bytes(pat.as_bytes(), target_bytes))
    }
}

fn expand_braces(pattern: &str) -> Vec<String> {
    let Some(open) = pattern.find('{') else {
        return vec![pattern.to_owned()];
    };
    let Some(rel_close) = pattern[open + 1..].find('}') else {
        return vec![pattern.to_owned()];
    };
    let close = open + 1 + rel_close;
    let prefix = &pattern[..open];
    let suffix = &pattern[close + 1..];
    let mut out = Vec::new();
    for item in pattern[open + 1..close].split(',') {
        let mut combined = String::with_capacity(prefix.len() + item.len() + suffix.len());
        combined.push_str(prefix);
        combined.push_str(item);
        combined.push_str(suffix);
        out.extend(expand_braces(&combined));
    }
    out
}

/// Non-exponential iterative/bounded glob matching over byte slices.
fn match_bytes(pat: &[u8], text: &[u8]) -> bool {
    let (mut pi, mut ti) = (0usize, 0usize);
    let mut star_pi: Option<usize> = None;
    let mut star_ti: usize = 0;
    let mut star_double = false;

    while ti < text.len() {
        if pi < pat.len() && pat[pi] == b'*' {
            let is_double = pi + 1 < pat.len() && pat[pi + 1] == b'*';
            if is_double {
                pi += 2;
                // Swallow optional `/` after `**` so `a/**/b` matches `a/b`
                if pi < pat.len() && pat[pi] == b'/' {
                    // Try matching directly without consuming a segment first
                    if match_bytes(&pat[pi + 1..], &text[ti..]) {
                        return true;
                    }
                    pi += 1;
                    // Advance to next '/' for backtracking
                    star_pi = Some(pi - 1);
                } else {
                    star_pi = Some(pi);
                }
                star_ti = ti;
                star_double = true;
            } else {
                pi += 1;
                star_pi = Some(pi);
                star_ti = ti;
                star_double = false;
            }
        } else if pi < pat.len() && pat[pi] == b'?' && text[ti] != b'/' {
            pi += 1;
            ti += 1;
        } else if pi < pat.len() && pat[pi] == b'[' {
            if let Some((matched, next_pi)) = match_char_class(pat, pi, text[ti])
                && matched
                && text[ti] != b'/'
            {
                pi = next_pi;
                ti += 1;
                continue;
            }
            if !backtrack(&mut pi, &mut ti, star_pi, &mut star_ti, star_double, text) {
                return false;
            }
        } else if pi < pat.len() && pat[pi] == text[ti] {
            pi += 1;
            ti += 1;
        } else if !backtrack(&mut pi, &mut ti, star_pi, &mut star_ti, star_double, text) {
            return false;
        }
    }

    while pi < pat.len() && pat[pi] == b'*' {
        pi += 1;
    }
    pi == pat.len()
}

#[inline]
fn backtrack(
    pi: &mut usize,
    ti: &mut usize,
    star_pi: Option<usize>,
    star_ti: &mut usize,
    star_double: bool,
    text: &[u8],
) -> bool {
    let Some(sp) = star_pi else {
        return false;
    };
    if !star_double && text[*star_ti] == b'/' {
        return false;
    }
    *star_ti += 1;
    *ti = *star_ti;
    *pi = sp;
    true
}

fn match_char_class(pat: &[u8], start: usize, ch: u8) -> Option<(bool, usize)> {
    let mut i = start + 1;
    if i >= pat.len() {
        return None;
    }
    let negated = pat[i] == b'!' || pat[i] == b'^';
    if negated {
        i += 1;
    }
    let mut matched = false;
    while i < pat.len() && pat[i] != b']' {
        if i + 2 < pat.len() && pat[i + 1] == b'-' && pat[i + 2] != b']' {
            if ch >= pat[i] && ch <= pat[i + 2] {
                matched = true;
            }
            i += 3;
        } else {
            if ch == pat[i] {
                matched = true;
            }
            i += 1;
        }
    }
    if i >= pat.len() {
        return None;
    }
    Some((if negated { !matched } else { matched }, i + 1))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn glob_matches_stars_double_stars_braces_and_char_classes() {
        let g = GlobPattern::compile("crates/**/src/*.{rs,toml}");
        assert!(g.matches("crates/core/src/lib.rs"));
        assert!(g.matches("crates\\runtime\\src\\vm.rs"));
        assert!(g.matches("crates/src/Cargo.toml"));
        assert!(!g.matches("crates/core/src/lib.ts"));

        let single = GlobPattern::compile("src/*.rs");
        assert!(single.matches("src/lib.rs"));
        assert!(!single.matches("src/nested/lib.rs"));

        let cls = GlobPattern::compile("v[0-9].?");
        assert!(cls.matches("v7.3"));
        assert!(!cls.matches("va.3"));
    }
}
