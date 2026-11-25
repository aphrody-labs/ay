//! Zero-allocation byte scanning, slicing, and UTF-8 decoding helpers
//! (mirrors `bun_core::strings` + Asterinas `dry` discipline).

use std::borrow::Cow;

/// Decodes `bytes` as a borrowed `&str` with zero heap allocation on valid UTF-8,
/// falling back to an out-of-line cold lossy decoder only on malformed sequences.
#[inline]
#[must_use]
pub fn decode_utf8(bytes: &[u8]) -> Cow<'_, str> {
    match std::str::from_utf8(bytes) {
        Ok(valid) => Cow::Borrowed(valid),
        Err(_) => Cow::Owned(decode_utf8_lossy_cold(bytes)),
    }
}

#[cold]
#[inline(never)]
fn decode_utf8_lossy_cold(mut bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len());
    loop {
        match std::str::from_utf8(bytes) {
            Ok(valid) => {
                out.push_str(valid);
                break;
            }
            Err(err) => {
                let (valid, after) = bytes.split_at(err.valid_up_to());
                if let Ok(s) = std::str::from_utf8(valid) {
                    out.push_str(s);
                }
                out.push('\u{FFFD}');
                match err.error_len() {
                    Some(n) => bytes = &after[n..],
                    None => break,
                }
            }
        }
    }
    out
}

/// Find the first index of `needle` in `haystack`.
#[inline]
#[must_use]
pub fn index_of_byte(haystack: &[u8], needle: u8) -> Option<usize> {
    haystack.iter().position(|&b| b == needle)
}

/// Find the last index of `needle` in `haystack`.
#[inline]
#[must_use]
pub fn last_index_of_byte(haystack: &[u8], needle: u8) -> Option<usize> {
    haystack.iter().rposition(|&b| b == needle)
}

/// Find the first index of byte slice `needle` inside `haystack` (zero allocation).
#[inline]
#[must_use]
pub fn index_of_bytes(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() {
        return Some(0);
    }
    let first = needle[0];
    let limit = haystack.len().checked_sub(needle.len())?;
    let mut pos = 0;
    while pos <= limit {
        let rel = index_of_byte(&haystack[pos..=limit], first)?;
        let candidate = pos + rel;
        if &haystack[candidate..candidate + needle.len()] == needle {
            return Some(candidate);
        }
        pos = candidate + 1;
    }
    None
}

/// Split `haystack` on the first occurrence of `delimiter` without allocating.
#[inline]
#[must_use]
pub fn split_once_byte(haystack: &[u8], delimiter: u8) -> Option<(&[u8], &[u8])> {
    let idx = index_of_byte(haystack, delimiter)?;
    Some((&haystack[..idx], &haystack[idx + 1..]))
}

/// Trim ASCII whitespace from both ends of a byte slice.
#[inline]
#[must_use]
pub fn trim_ascii(mut bytes: &[u8]) -> &[u8] {
    while let Some((&first, rest)) = bytes.split_first() {
        if first.is_ascii_whitespace() {
            bytes = rest;
        } else {
            break;
        }
    }
    while let Some((&last, rest)) = bytes.split_last() {
        if last.is_ascii_whitespace() {
            bytes = rest;
        } else {
            break;
        }
    }
    bytes
}

/// Case-insensitive ASCII prefix match (zero allocation).
#[inline]
#[must_use]
pub fn starts_with_ignore_ascii_case(haystack: &[u8], prefix: &[u8]) -> bool {
    haystack.len() >= prefix.len() && haystack[..prefix.len()].eq_ignore_ascii_case(prefix)
}

/// Zero-allocation parser for `[[tree]]` pin blocks in `crates/os/linux/kernel.toml`.
///
/// Returns the unquoted `tag` field (e.g. `"v7.3.5"` or `"cachyos-7.3.5-2"`) for the entry
/// whose `id` matches `tree_id`. Shared across `aphrody-uutils`, `aphrody-unixwin`,
/// `aphrody-linux-wasm`, and `aphrody-linux` to prevent duplicate hand-rolled parsers.
#[inline]
#[must_use]
pub fn pinned_kernel_tag<'a>(kernel_toml: &'a str, tree_id: &str) -> Option<&'a str> {
    let mut in_match = false;
    for raw in kernel_toml.lines() {
        let line = raw.trim();
        if line == "[[tree]]" {
            if in_match {
                return None;
            }
            in_match = false;
            continue;
        }
        if let Some(rest) = line.strip_prefix("id")
            && let Some((_, rhs)) = rest.split_once('=')
        {
            in_match = rhs.trim().trim_matches('"') == tree_id;
            continue;
        }
        if in_match
            && let Some(rest) = line.strip_prefix("tag")
            && let Some((_, rhs)) = rest.split_once('=')
        {
            let tag = rhs.trim().trim_matches('"');
            if !tag.is_empty() {
                return Some(tag);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn byte_helpers_work_zero_alloc() {
        assert_eq!(index_of_byte(b"aphrody-core", b'-'), Some(7));
        assert_eq!(last_index_of_byte(b"a/b/c", b'/'), Some(3));
        assert_eq!(index_of_bytes(b"linux-7.3.5-cachyos", b"cachy"), Some(12));
        assert_eq!(index_of_bytes(b"linux", b"wasm"), None);
        assert_eq!(
            split_once_byte(b"key=value=extra", b'='),
            Some((&b"key"[..], &b"value=extra"[..]))
        );
        assert_eq!(trim_ascii(b"  \t aphrody \r\n "), b"aphrody");
        assert!(starts_with_ignore_ascii_case(
            b"Content-Type: wasm",
            b"content-type:"
        ));

        let sample = "[[tree]]\nid = \"stable\"\ntag = \"v7.3.5\"\n[[tree]]\nid = \"cachyos\"\ntag = \"cachyos-7.3.5-2\"\n";
        assert_eq!(pinned_kernel_tag(sample, "stable"), Some("v7.3.5"));
        assert_eq!(
            pinned_kernel_tag(sample, "cachyos"),
            Some("cachyos-7.3.5-2")
        );
        assert_eq!(pinned_kernel_tag(sample, "missing"), None);
    }
}
