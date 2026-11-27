//! The C ABI of `src/jsc/bindings/NumberConversions.cpp` and `bun_icu_normalize.cpp` that `bun_core` links
//! against, in Rust: `Number.prototype.toFixed`/`toPrecision`/`toExponential` from the exact decimal value
//! of the double, and normalization on ICU4X.

/// The decimal digits of `x` (finite, not negative) without leading zeros, and the power of ten of the first
/// one. `("", 0)` for zero.
fn exact(x: f64) -> (Vec<u8>, i32) {
    // 1074 fraction digits hold every double exactly.
    let text = format!("{x:.1074}");
    let (int, frac) = text.split_once('.').unwrap_or((&text, ""));
    let all: Vec<u8> = int.bytes().chain(frac.bytes()).collect();
    let Some(first) = all.iter().position(|&d| d != b'0') else {
        return (Vec::new(), 0);
    };
    let exponent = int.len() as i32 - 1 - first as i32;
    let mut digits = all[first..].to_vec();
    while digits.last() == Some(&b'0') {
        digits.pop();
    }
    (digits, exponent)
}

/// `digits` rounded to `n` (at least 0) of them, half away from zero, as the spec's "the larger n". The
/// exponent goes up by one if they carry over.
fn round(digits: &[u8], exponent: i32, n: usize) -> (Vec<u8>, i32) {
    let mut out: Vec<u8> = digits.iter().copied().take(n).collect();
    out.resize(n, b'0');
    if digits.get(n).is_some_and(|&d| d >= b'5') {
        let mut i = n;
        loop {
            if i == 0 {
                out.insert(0, b'1');
                out.truncate(n.max(1));
                return (out, exponent + 1);
            }
            i -= 1;
            if out[i] == b'9' {
                out[i] = b'0';
            } else {
                out[i] += 1;
                break;
            }
        }
    }
    (out, exponent)
}

fn write(buf: &mut [u8; 124], text: &[u8]) -> usize {
    let n = text.len().min(buf.len());
    buf[..n].copy_from_slice(&text[..n]);
    n
}

fn sign(number: f64) -> &'static [u8] {
    if number < 0.0 { b"-" } else { b"" }
}

/// `d.ddd` + `e+X`
fn exponential(number: f64, digits: &[u8], exponent: i32) -> Vec<u8> {
    let mut out = sign(number).to_vec();
    out.push(digits[0]);
    if digits.len() > 1 {
        out.push(b'.');
        out.extend_from_slice(&digits[1..]);
    }
    out.extend_from_slice(
        format!(
            "e{}{}",
            if exponent < 0 { '-' } else { '+' },
            exponent.abs()
        )
        .as_bytes(),
    );
    out
}

#[unsafe(no_mangle)]
pub extern "C" fn WTF__numberToFixed(
    buf: &mut [u8; 124],
    number: f64,
    fraction_digits: u32,
) -> usize {
    let f = fraction_digits as i32;
    let (digits, exponent) = exact(number.abs());
    let keep = exponent + 1 + f;
    let (rounded, exponent) = if digits.is_empty() || keep < 0 {
        (Vec::new(), 0)
    } else {
        round(&digits, exponent, keep as usize)
    };
    // The value is `rounded` × 10^(exponent + 1 - len): write it with exactly `f` fraction digits.
    let mut scaled: Vec<u8> = if rounded.iter().all(|&d| d == b'0') {
        Vec::new()
    } else {
        let mut s = rounded.clone();
        let len = s.len() as i32;
        let shift = exponent + 1 - len + f;
        s.extend(std::iter::repeat_n(b'0', shift.max(0) as usize));
        s
    };
    let width = f as usize + 1;
    if scaled.len() < width {
        scaled.splice(0..0, std::iter::repeat_n(b'0', width - scaled.len()));
    }
    let point = scaled.len() - f as usize;
    let mut out = sign(number).to_vec();
    out.extend_from_slice(&scaled[..point]);
    if f > 0 {
        out.push(b'.');
        out.extend_from_slice(&scaled[point..]);
    }
    write(buf, &out)
}

#[unsafe(no_mangle)]
pub extern "C" fn WTF__numberToPrecision(
    buf: &mut [u8; 124],
    number: f64,
    precision: u32,
) -> usize {
    let p = precision as usize;
    let (digits, exponent) = exact(number.abs());
    let (digits, exponent) = if digits.is_empty() {
        (vec![b'0'; p], 0)
    } else {
        round(&digits, exponent, p)
    };
    if exponent < -6 || exponent >= p as i32 {
        return write(buf, &exponential(number, &digits, exponent));
    }
    let mut out = sign(number).to_vec();
    if exponent < 0 {
        out.extend_from_slice(b"0.");
        out.extend(std::iter::repeat_n(b'0', (-exponent - 1) as usize));
        out.extend_from_slice(&digits);
    } else {
        let point = exponent as usize + 1;
        out.extend_from_slice(&digits[..point]);
        if point < p {
            out.push(b'.');
            out.extend_from_slice(&digits[point..]);
        }
    }
    write(buf, &out)
}

#[unsafe(no_mangle)]
pub extern "C" fn WTF__numberToExponential(
    buf: &mut [u8; 124],
    number: f64,
    fraction_digits: i32,
) -> usize {
    let (digits, exponent) = if fraction_digits < 0 {
        // As many as it takes: the shortest that reads back, which `{:e}` prints.
        let text = format!("{:e}", number.abs());
        let (mantissa, exponent) = text.split_once('e').unwrap_or((&text, "0"));
        (
            mantissa.bytes().filter(|&b| b != b'.').collect(),
            exponent.parse().unwrap_or(0),
        )
    } else {
        let n = fraction_digits as usize + 1;
        let (digits, exponent) = exact(number.abs());
        if digits.is_empty() {
            (vec![b'0'; n], 0)
        } else {
            round(&digits, exponent, n)
        }
    };
    write(buf, &exponential(number, &digits, exponent))
}

/// # Safety
/// `text` has `length` units and `out` room for `capacity`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn Bun__normalizeUTF16(
    form: u8,
    text: *const u16,
    length: usize,
    out: *mut u16,
    capacity: usize,
) -> i32 {
    use icu_normalizer::{ComposingNormalizerBorrowed as C, DecomposingNormalizerBorrowed as D};
    // SAFETY: the caller's.
    let text = if length == 0 {
        &[][..]
    } else {
        unsafe { core::slice::from_raw_parts(text, length) }
    };
    let normalize = |run: &[u16]| -> Vec<u16> {
        match form {
            0 => C::new_nfc().normalize_utf16(run).into_owned(),
            1 => D::new_nfd().normalize_utf16(run).into_owned(),
            2 => C::new_nfkc().normalize_utf16(run).into_owned(),
            _ => D::new_nfkd().normalize_utf16(run).into_owned(),
        }
    };
    // ICU4X turns an unpaired surrogate into U+FFFD; ICU4C's `unorm2_normalize`, which this stands in for,
    // keeps it. It is a starter that nothing composes with, so a boundary: normalize the well-formed runs
    // between them and keep each as it is.
    let mut normalized = Vec::with_capacity(text.len());
    let mut run_start = 0;
    let mut i = 0;
    while i < text.len() {
        let unit = text[i];
        let paired = match unit {
            0xD800..=0xDBFF => text
                .get(i + 1)
                .is_some_and(|&next| (0xDC00..=0xDFFF).contains(&next)),
            0xDC00..=0xDFFF => false,
            _ => {
                i += 1;
                continue;
            }
        };
        if paired {
            i += 2;
            continue;
        }
        normalized.extend(normalize(&text[run_start..i]));
        normalized.push(unit);
        i += 1;
        run_start = i;
    }
    normalized.extend(normalize(&text[run_start..]));
    if normalized.len() <= capacity && capacity > 0 {
        // SAFETY: the caller's.
        unsafe { core::ptr::copy_nonoverlapping(normalized.as_ptr(), out, normalized.len()) };
    }
    i32::try_from(normalized.len()).unwrap_or(-1)
}

#[cfg(test)]
mod tests {
    fn call(f: impl Fn(&mut [u8; 124]) -> usize) -> String {
        let mut buf = [0; 124];
        let n = f(&mut buf);
        String::from_utf8(buf[..n].to_vec()).unwrap()
    }

    fn normalize(form: u8, text: &[u16]) -> Vec<u16> {
        let mut out = vec![0u16; 64];
        // SAFETY: `text` and `out` have those lengths.
        let n = unsafe {
            super::Bun__normalizeUTF16(form, text.as_ptr(), text.len(), out.as_mut_ptr(), out.len())
        };
        out.truncate(n as usize);
        out
    }

    #[test]
    fn normalization() {
        let u = |s: &str| s.encode_utf16().collect::<Vec<_>>();
        // "e" + COMBINING ACUTE <-> "é"; "ﬁ" -> "fi" only for the K forms.
        assert_eq!(normalize(0, &u("e\u{301}")), u("\u{e9}"));
        assert_eq!(normalize(1, &u("\u{e9}")), u("e\u{301}"));
        assert_eq!(normalize(2, &u("\u{fb01}")), u("fi"));
        assert_eq!(normalize(0, &u("\u{fb01}")), u("\u{fb01}"));
        assert_eq!(normalize(3, &u("\u{1e9b}\u{323}")), u("s\u{323}\u{307}"));
        // Pairs stay pairs.
        assert_eq!(normalize(0, &u("\u{1f600}")), u("\u{1f600}"));
        // Unpaired surrogates stay as they are, as '\uD800'.normalize() in JSC.
        for form in 0..4 {
            assert_eq!(normalize(form, &[0xD800]), [0xD800]);
            assert_eq!(normalize(form, &[0xDC00, 0x61]), [0xDC00, 0x61]);
        }
        assert_eq!(
            normalize(0, &[0x65, 0x301, 0xD800, 0x65, 0x301, 0xDFFF]),
            [0xE9, 0xD800, 0xE9, 0xDFFF]
        );
        assert_eq!(normalize(1, &[0xD83D, 0xE9]), [0xD83D, 0x65, 0x301]);
        // Too little room: the length it needs, nothing written.
        let mut out = [0u16; 1];
        let text = u("\u{e9}");
        // SAFETY: as above.
        assert_eq!(
            unsafe { super::Bun__normalizeUTF16(1, text.as_ptr(), 1, out.as_mut_ptr(), 1) },
            2
        );
    }

    #[test]
    fn like_javascript() {
        use super::*;
        let fixed = |x, f| call(|b| WTF__numberToFixed(b, x, f));
        assert_eq!(fixed(2.5, 0), "3");
        assert_eq!(fixed(1.005, 2), "1.00");
        assert_eq!(fixed(-1.5, 0), "-2");
        assert_eq!(fixed(0.0, 2), "0.00");
        assert_eq!(fixed(-0.0001, 2), "-0.00");
        assert_eq!(fixed(123.456, 1), "123.5");
        assert_eq!(fixed(0.000001, 7), "0.0000010");
        assert_eq!(fixed(99.99, 1), "100.0");
        let precision = |x, p| call(|b| WTF__numberToPrecision(b, x, p));
        assert_eq!(precision(123.456, 4), "123.5");
        assert_eq!(precision(0.00001, 1), "0.00001");
        assert_eq!(precision(1e-7, 1), "1e-7");
        assert_eq!(precision(123456.0, 2), "1.2e+5");
        assert_eq!(precision(0.0, 3), "0.00");
        assert_eq!(precision(99.99, 3), "100");
        let expo = |x, f| call(|b| WTF__numberToExponential(b, x, f));
        assert_eq!(expo(123456.0, 2), "1.23e+5");
        assert_eq!(expo(123456.0, -1), "1.23456e+5");
        assert_eq!(expo(0.0, -1), "0e+0");
        assert_eq!(expo(-0.00015, 1), "-1.5e-4");
        assert_eq!(expo(9.99, 1), "1.0e+1");
    }
}
