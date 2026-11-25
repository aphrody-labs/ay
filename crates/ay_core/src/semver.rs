//! Pure-Rust SemVer 2.0 parser, comparator, and range matcher
//! (modeled on `oven-sh/bun`'s `bun_semver`, 12 dependents).

use std::{cmp::Ordering, fmt};

use crate::error::{CoreError, CoreResult};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Version {
    pub major: u64,
    pub minor: u64,
    pub patch: u64,
    pub pre: Option<String>,
    pub build: Option<String>,
}

impl Version {
    pub fn parse(input: &str) -> CoreResult<Self> {
        let trimmed = input.trim();
        let s = trimmed.strip_prefix('v').unwrap_or(trimmed);
        let (core_and_pre, build) = match s.split_once('+') {
            Some((a, b)) => (a, Some(b.to_owned())),
            None => (s, None),
        };
        let (core, pre) = match core_and_pre.split_once('-') {
            Some((a, b)) => (a, Some(b.to_owned())),
            None => (core_and_pre, None),
        };
        let mut parts = core.split('.');
        let major = parse_num(parts.next(), "major")?;
        let minor = parse_num(parts.next(), "minor")?;
        let patch = parse_num(parts.next(), "patch")?;
        if parts.next().is_some() {
            return Err(CoreError::invalid_arg(
                "semver",
                "too many numeric components",
            ));
        }
        Ok(Self {
            major,
            minor,
            patch,
            pre,
            build,
        })
    }
}

fn parse_num(part: Option<&str>, field: &'static str) -> CoreResult<u64> {
    let Some(s) = part else {
        return Err(CoreError::invalid_arg("semver", format!("missing {field}")));
    };
    if s.is_empty() || (s.len() > 1 && s.starts_with('0')) {
        return Err(CoreError::invalid_arg(
            "semver",
            format!("invalid {field}: `{s}`"),
        ));
    }
    s.parse::<u64>()
        .map_err(|_| CoreError::invalid_arg("semver", format!("non-numeric {field}: `{s}`")))
}

impl Ord for Version {
    #[inline]
    fn cmp(&self, other: &Self) -> Ordering {
        self.major
            .cmp(&other.major)
            .then_with(|| self.minor.cmp(&other.minor))
            .then_with(|| self.patch.cmp(&other.patch))
            .then_with(|| match (&self.pre, &other.pre) {
                (None, None) => Ordering::Equal,
                (None, Some(_)) => Ordering::Greater,
                (Some(_), None) => Ordering::Less,
                (Some(a), Some(b)) => a.cmp(b),
            })
    }
}

impl PartialOrd for Version {
    #[inline]
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)?;
        if let Some(pre) = &self.pre {
            write!(f, "-{pre}")?;
        }
        if let Some(build) = &self.build {
            write!(f, "+{build}")?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VersionReq {
    Any,
    Exact(Version),
    Caret(Version),
    Tilde(Version),
    Gte(Version),
    Gt(Version),
    Lte(Version),
    Lt(Version),
}

impl VersionReq {
    pub fn parse(input: &str) -> CoreResult<Self> {
        let s = input.trim();
        if s == "*" || s.is_empty() {
            return Ok(Self::Any);
        }
        if let Some(rest) = s.strip_prefix('^') {
            return Ok(Self::Caret(Version::parse(rest)?));
        }
        if let Some(rest) = s.strip_prefix('~') {
            return Ok(Self::Tilde(Version::parse(rest)?));
        }
        if let Some(rest) = s.strip_prefix(">=") {
            return Ok(Self::Gte(Version::parse(rest)?));
        }
        if let Some(rest) = s.strip_prefix("<=") {
            return Ok(Self::Lte(Version::parse(rest)?));
        }
        if let Some(rest) = s.strip_prefix('>') {
            return Ok(Self::Gt(Version::parse(rest)?));
        }
        if let Some(rest) = s.strip_prefix('<') {
            return Ok(Self::Lt(Version::parse(rest)?));
        }
        let rest = s.strip_prefix('=').unwrap_or(s);
        Ok(Self::Exact(Version::parse(rest)?))
    }

    #[inline]
    #[must_use]
    pub fn matches(&self, v: &Version) -> bool {
        match self {
            Self::Any => true,
            Self::Exact(req) => v == req,
            Self::Gte(req) => v >= req,
            Self::Gt(req) => v > req,
            Self::Lte(req) => v <= req,
            Self::Lt(req) => v < req,
            Self::Tilde(req) => v >= req && v.major == req.major && v.minor == req.minor,
            Self::Caret(req) => {
                if v < req {
                    return false;
                }
                if req.major > 0 {
                    v.major == req.major
                } else if req.minor > 0 {
                    v.major == 0 && v.minor == req.minor
                } else {
                    v.major == 0 && v.minor == 0 && v.patch == req.patch
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn semver_ordering_and_ranges_match_spec() {
        let a = Version::parse("7.3.5-rc1").expect("parse pre");
        let b = Version::parse("7.3.5").expect("parse release");
        let c = Version::parse("7.4.0").expect("parse next minor");
        assert!(a < b && b < c);
        assert!(Version::parse("01.2.3").is_err());

        let caret = VersionReq::parse("^7.3.0").expect("caret");
        assert!(caret.matches(&b));
        assert!(caret.matches(&c));
        assert!(!caret.matches(&Version::parse("8.0.0").expect("v8")));

        let tilde = VersionReq::parse("~7.3.0").expect("tilde");
        assert!(tilde.matches(&b));
        assert!(!tilde.matches(&c));
    }
}
