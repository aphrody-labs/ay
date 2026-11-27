//! The C ABI of `src/jsc/bindings/RegularExpression.cpp` that `bun_yarr` links against, on `regress` (Boa's
//! regular expressions) instead of JavaScriptCore's Yarr. `bun lint` reaches it through `bun_lint`.
//!
//! Every symbol here is weak: if RegularExpression.cpp or `bun_jsc` (which defines `__bun_yarr_initialize`)
//! is ever linked too, theirs win instead of the link failing on a duplicate.

use core::ffi::c_void;

/// `bun_yarr::flags`
const IGNORE_CASE: u16 = 1 << 2;
const MULTILINE: u16 = 1 << 3;
const DOT_ALL: u16 = 1 << 4;
const UNICODE: u16 = 1 << 5;
const UNICODE_SETS: u16 = 1 << 6;
const STICKY: u16 = 1 << 7;

/// `bun_yarr::NO_MATCH`
const NO_MATCH: u32 = u32::MAX;

/// `Refusal` in RegularExpression.cpp.
const TOO_LARGE: u8 = 1;
const UNKNOWN_PROPERTY: u8 = 2;
const SYNTAX: u8 = 3;

/// regress's messages (`parse.rs` of regress 0.12) for what Yarr calls `PatternTooLarge`, `QuantifierTooLarge` and
/// `TooManyDisjunctions`, and for `InvalidUnicodePropertyExpression`. regress has no error kinds, only text: each
/// message is matched whole, and anything else is a syntax error.
const TOO_LARGE_MESSAGES: &[&str] = &[
    "Regular expression is too deeply nested",
    "Capture group count limit exceeded",
    "Loop count limit exceeded",
];
const UNKNOWN_PROPERTY_MESSAGES: &[&str] = &["Invalid property name"];

/// `WTF::String::MaxLength`, Yarr's `PatternTooLarge` for the pattern itself.
const MAX_PATTERN: u32 = i32::MAX as u32;

fn refusal(message: &str) -> u8 {
    if TOO_LARGE_MESSAGES.contains(&message) {
        TOO_LARGE
    } else if UNKNOWN_PROPERTY_MESSAGES.contains(&message) {
        UNKNOWN_PROPERTY
    } else {
        SYNTAX
    }
}

struct Expression {
    regex: regress::Regex,
    unicode: bool,
    sticky: bool,
    /// What compile told the caller: exec writes no further than this.
    offsets_size: usize,
}

/// # Safety
/// `characters` has `length` characters, of one byte if `is_8_bit`, of two otherwise.
unsafe fn units<'a>(
    characters: *const c_void,
    length: u32,
    is_8_bit: bool,
) -> std::borrow::Cow<'a, [u16]> {
    if length == 0 {
        return std::borrow::Cow::Borrowed(&[]);
    }
    if is_8_bit {
        // SAFETY: the caller's.
        let bytes =
            unsafe { core::slice::from_raw_parts(characters.cast::<u8>(), length as usize) };
        std::borrow::Cow::Owned(bytes.iter().map(|&b| u16::from(b)).collect())
    } else {
        // SAFETY: the caller's.
        std::borrow::Cow::Borrowed(unsafe {
            core::slice::from_raw_parts(characters.cast::<u16>(), length as usize)
        })
    }
}

/// The code points of a pattern: pairs of surrogates are one only with the `u` or `v` flag.
fn code_points(units: &[u16], unicode: bool) -> Vec<u32> {
    if !unicode {
        return units.iter().map(|&u| u32::from(u)).collect();
    }
    char::decode_utf16(units.iter().copied())
        .map(|it| it.map_or_else(|e| u32::from(e.unpaired_surrogate()), u32::from))
        .collect()
}

/// # Safety
/// As `Yarr__RegularExpression__compile` in RegularExpression.cpp.
#[unsafe(no_mangle)]
#[linkage = "weak"]
pub unsafe extern "C" fn Yarr__RegularExpression__compile(
    characters: *const c_void,
    length: u32,
    is_8_bit: bool,
    flags: u16,
    offsets_size: &mut u32,
    refusal_out: &mut u8,
) -> *mut c_void {
    if length > MAX_PATTERN {
        *refusal_out = TOO_LARGE;
        return core::ptr::null_mut();
    }
    // SAFETY: the caller's.
    let pattern = unsafe { units(characters, length, is_8_bit) };
    let unicode = flags & (UNICODE | UNICODE_SETS) != 0;
    let options = regress::Flags {
        icase: flags & IGNORE_CASE != 0,
        multiline: flags & MULTILINE != 0,
        dot_all: flags & DOT_ALL != 0,
        unicode: flags & UNICODE != 0,
        unicode_sets: flags & UNICODE_SETS != 0,
        no_opt: false,
    };
    let points = code_points(&pattern, unicode);
    match regress::Regex::from_unicode(points.into_iter(), options) {
        Ok(regex) => {
            // Two for the match and two for each group the parser counted, as Yarr's `m_offsetsSize`. regress
            // refuses more than 65535 groups, so this fits.
            let size = 2 * (regex.captures_len() + 1);
            *offsets_size = size as u32;
            let sticky = flags & STICKY != 0;
            Box::into_raw(Box::new(Expression {
                regex,
                unicode,
                sticky,
                offsets_size: size,
            }))
            .cast()
        }
        Err(err) => {
            *refusal_out = refusal(&err.text);
            core::ptr::null_mut()
        }
    }
}

/// # Safety
/// As `Yarr__RegularExpression__exec` in RegularExpression.cpp: `offsets` has room for the `offsets_size` of
/// the compile that made `this`.
#[unsafe(no_mangle)]
#[linkage = "weak"]
pub unsafe extern "C" fn Yarr__RegularExpression__exec(
    this: *mut c_void,
    characters: *const c_void,
    length: u32,
    is_8_bit: bool,
    start: u32,
    offsets: *mut u32,
) -> bool {
    // As Yarr: a start past the end matches nothing.
    if start > length {
        return false;
    }
    // SAFETY: made by compile.
    let this = unsafe { &*this.cast::<Expression>() };
    // SAFETY: the caller's.
    let text = unsafe { units(characters, length, is_8_bit) };
    // A panic must not unwind out of `extern "C"`: it counts as no match.
    let found = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        if this.unicode {
            this.regex.find_from_utf16(&text, start as usize).next()
        } else {
            this.regex.find_from_ucs2(&text, start as usize).next()
        }
    }))
    .ok()
    .flatten();
    let Some(found) = found.filter(|m| !this.sticky || m.range.start == start as usize) else {
        return false;
    };
    // SAFETY: the caller's: room for the `offsets_size` that compile gave, a pair for the match and each capture.
    let offsets = unsafe { core::slice::from_raw_parts_mut(offsets, this.offsets_size) };
    let ranges = core::iter::once(Some(found.range.clone())).chain(found.captures.iter().cloned());
    for (slot, range) in offsets.chunks_exact_mut(2).zip(ranges) {
        match range {
            Some(range) => {
                slot[0] = range.start as u32;
                slot[1] = range.end as u32;
            }
            None => slot[0] = NO_MATCH,
        }
    }
    true
}

/// # Safety
/// `this` is from compile and nothing uses it after.
#[unsafe(no_mangle)]
#[linkage = "weak"]
pub unsafe extern "C" fn Yarr__RegularExpression__deinit(this: *mut c_void) {
    // SAFETY: the caller's.
    drop(unsafe { Box::from_raw(this.cast::<Expression>()) });
}

/// Nothing to start: regress has no options to read. `bun_jsc`'s, which starts JavaScriptCore, wins if linked.
#[unsafe(no_mangle)]
#[linkage = "weak"]
pub fn __bun_yarr_initialize() {}

#[cfg(test)]
mod tests {
    use super::*;

    fn compile(pattern: &str, flags: u16) -> Result<(*mut c_void, u32), u8> {
        let units: Vec<u16> = pattern.encode_utf16().collect();
        let (mut size, mut refusal) = (0, 0);
        // SAFETY: `units` has that many.
        let this = unsafe {
            Yarr__RegularExpression__compile(
                units.as_ptr().cast(),
                units.len() as u32,
                false,
                flags,
                &mut size,
                &mut refusal,
            )
        };
        if this.is_null() {
            Err(refusal)
        } else {
            Ok((this, size))
        }
    }

    fn exec(this: *mut c_void, text: &str, start: u32, offsets: &mut [u32]) -> bool {
        let units: Vec<u16> = text.encode_utf16().collect();
        // SAFETY: `units` has that many, `offsets` has room.
        unsafe {
            Yarr__RegularExpression__exec(
                this,
                units.as_ptr().cast(),
                units.len() as u32,
                false,
                start,
                offsets.as_mut_ptr(),
            )
        }
    }

    #[test]
    fn offsets_size_counts_capturing_groups_only() {
        for (pattern, flags, groups) in [
            ("a", 0, 0),
            ("(a)(b)", 0, 2),
            (r"\((a)", 0, 1),
            ("(?:a)(?=b)(?!c)(?<=d)(?<!e)(?i:f)", 0, 0),
            ("(?<name>a)", 0, 1),
            ("[(](a)", 0, 1),
            (r"[\]\(](a)", 0, 1),
            (r"[[\(]](a)", UNICODE_SETS, 1),
            (r"[[a]--[\(]](a)", UNICODE_SETS, 1),
        ] {
            let (this, size) =
                compile(pattern, flags).unwrap_or_else(|r| panic!("{pattern}: refusal {r}"));
            assert_eq!(size, 2 * (groups + 1), "{pattern}");
            // SAFETY: from compile.
            unsafe { Yarr__RegularExpression__deinit(this) };
        }
    }

    #[test]
    fn refusals() {
        assert_eq!(compile("(", 0).unwrap_err(), SYNTAX);
        // As JavaScriptCore: with `v` a `(` in a class has to be escaped.
        assert_eq!(compile("[[(]](a)", UNICODE_SETS).unwrap_err(), SYNTAX);
        assert_eq!(
            compile(r"\p{NotAProperty}", UNICODE).unwrap_err(),
            UNKNOWN_PROPERTY
        );
        assert_eq!(compile(&"a*".repeat(70_000), 0).unwrap_err(), TOO_LARGE);
        assert_eq!(compile(&"(".repeat(100_000), 0).unwrap_err(), TOO_LARGE);
    }

    #[test]
    fn exec_matches_and_start_past_the_end() {
        let (this, size) = compile("a(b)?", 0).unwrap();
        let mut offsets = vec![7; size as usize];
        assert!(exec(this, "xab", 0, &mut offsets));
        assert_eq!(offsets, [1, 3, 2, 3]);
        assert!(!exec(this, "xab", 4, &mut offsets));
        assert!(!exec(this, "xab", u32::MAX, &mut offsets));
        assert!(exec(this, "a", 0, &mut offsets));
        assert_eq!(offsets[2], NO_MATCH);
        // SAFETY: from compile.
        unsafe { Yarr__RegularExpression__deinit(this) };
    }
}
