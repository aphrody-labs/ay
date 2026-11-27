//! What `bun_core` and `bun_url` take from WTF, in Rust: strings (`BunString__from*`, `Bun__WTFStringImpl__destroy`
//! of src/jsc/bindings/BunString.cpp), `WTF::URL` (`URL__*`, on the `url` crate's WHATWG parser), and the date parser
//! behind `Bun__parseDateTimeString` (wtf-bindings.cpp, V8's `ParseDateTimeString`).

use core::ffi::c_void;
use core::sync::atomic::AtomicU32;

use bun_alloc::WTFStringImplStruct;
use bun_core::{EncodedSlice, String as BunString};

// ── strings ────────────────────────────────────────────────────────────────

/// `WTF::StringImpl`'s head, as `bun_alloc::WTFStringImplStruct` reads it, with the characters after it.
#[repr(C)]
struct StringImpl {
    ref_count: AtomicU32,
    length: u32,
    characters: *const u8,
    hash_and_flags: u32,
}

const _: () = assert!(core::mem::size_of::<StringImpl>() == core::mem::size_of::<WTFStringImplStruct>());

/// `s_hashFlag8BitBuffer`.
const HASH_FLAG_8BIT_BUFFER: u32 = 1 << 2;
/// One ref: `s_refCountIncrement`.
const ONE_REF: u32 = 2;

fn layout(length: usize, is_8bit: bool) -> std::alloc::Layout {
    let bytes = if is_8bit { length } else { length * 2 };
    std::alloc::Layout::from_size_align(core::mem::size_of::<StringImpl>() + bytes, 8).expect("string size")
}

/// A new impl with one ref, its characters copied from `bytes` (`length` units of 1 or 2 bytes).
fn create(bytes: &[u8], length: usize, is_8bit: bool) -> BunString {
    if length == 0 {
        return BunString::EMPTY;
    }
    let layout = layout(length, is_8bit);
    // SAFETY: the layout is not empty; the characters follow the head.
    unsafe {
        let p = std::alloc::alloc(layout).cast::<StringImpl>();
        if p.is_null() {
            std::alloc::handle_alloc_error(layout);
        }
        let characters = p.add(1).cast::<u8>();
        core::ptr::copy_nonoverlapping(bytes.as_ptr(), characters, bytes.len());
        p.write(StringImpl {
            ref_count: AtomicU32::new(ONE_REF),
            length: u32::try_from(length).expect("a string of at most 2^32 - 1 characters"),
            characters,
            hash_and_flags: if is_8bit { HASH_FLAG_8BIT_BUFFER } else { 0 },
        });
        BunString::adopt_wtf_impl(p.cast())
    }
}

fn create_latin1(latin1: &[u8]) -> BunString {
    create(latin1, latin1.len(), true)
}

fn create_utf16(units: &[u16]) -> BunString {
    create(bun_core::slice_as_bytes(units), units.len(), false)
}

/// # Safety
/// `ptr` and `len` are those of a slice, or `len` is 0.
unsafe fn slice<'a, T>(ptr: *const T, len: usize) -> &'a [T] {
    if len == 0 { &[] } else { unsafe { core::slice::from_raw_parts(ptr, len) } }
}

/// `WTF::String::fromUTF8`: ASCII stays 8-bit, the rest becomes UTF-16; invalid UTF-8 is replaced.
#[unsafe(no_mangle)]
unsafe extern "C" fn BunString__fromBytes(bytes: *const u8, len: usize) -> BunString {
    // SAFETY: the caller's slice.
    let bytes = unsafe { slice(bytes, len) };
    if bytes.is_ascii() {
        return create_latin1(bytes);
    }
    let units: Vec<u16> = String::from_utf8_lossy(bytes).encode_utf16().collect();
    create_utf16(&units)
}

#[unsafe(no_mangle)]
unsafe extern "C" fn BunString__fromLatin1(bytes: *const u8, len: usize) -> BunString {
    // SAFETY: the caller's slice.
    create_latin1(unsafe { slice(bytes, len) })
}

#[unsafe(no_mangle)]
unsafe extern "C" fn BunString__fromUTF16(units: *const u16, len: usize) -> BunString {
    // SAFETY: the caller's slice.
    create_utf16(unsafe { slice(units, len) })
}

/// The caller knows every unit is below 256.
#[unsafe(no_mangle)]
unsafe extern "C" fn BunString__fromUTF16ToLatin1(units: *const u16, len: usize) -> BunString {
    // SAFETY: the caller's slice.
    let latin1: Vec<u8> = unsafe { slice(units, len) }.iter().map(|&u| u as u8).collect();
    create_latin1(&latin1)
}

/// The last ref went: free the impl and its characters, which `create` allocated together.
#[unsafe(no_mangle)]
unsafe extern "C" fn Bun__WTFStringImpl__destroy(this: *const WTFStringImplStruct) {
    // SAFETY: an impl from `create`, whose last ref the caller dropped.
    unsafe {
        let p = this.cast::<StringImpl>().cast_mut();
        let is_8bit = (*p).hash_and_flags & HASH_FLAG_8BIT_BUFFER != 0;
        std::alloc::dealloc(p.cast(), layout((*p).length as usize, is_8bit));
    }
}

// ── URL ────────────────────────────────────────────────────────────────────

/// `bun_url::whatwg::URL`'s 40 bytes of storage hold a `Box<url::Url>`.
type Storage = *mut url::Url;

fn text(input: &BunString) -> std::string::String {
    String::from_utf8_lossy(&input.to_utf8()).into_owned()
}

/// `WTF::URL::string()`: the href, or `Dead` for what does not parse.
fn href_of(url: Option<url::Url>) -> BunString {
    match url {
        Some(url) if !url.as_str().is_empty() => BunString::clone_utf8(url.as_str().as_bytes()),
        _ => BunString::DEAD,
    }
}

#[unsafe(no_mangle)]
extern "C" fn URL__getHref(input: &BunString) -> BunString {
    href_of(url::Url::parse(&text(input)).ok())
}

#[unsafe(no_mangle)]
extern "C" fn URL__getHrefJoin(base: &BunString, relative: &BunString) -> BunString {
    href_of(url::Url::parse(&text(base)).and_then(|base| base.join(&text(relative))).ok())
}

/// Leaves a URL in `storage` exactly when it returns true.
#[unsafe(no_mangle)]
unsafe extern "C" fn URL__fromString(storage: *mut c_void, input: &BunString, base: *const c_void) -> bool {
    let parsed = if base.is_null() {
        url::Url::parse(&text(input))
    } else {
        // SAFETY: `base` is a URL that `URL__fromString` left.
        let base = unsafe { &**base.cast::<Storage>() };
        base.join(&text(input))
    };
    match parsed {
        Ok(url) => {
            // SAFETY: the caller's storage of 40 bytes, aligned to 8.
            unsafe { storage.cast::<Storage>().write(Box::into_raw(Box::new(url))) };
            true
        }
        Err(_) => false,
    }
}

/// # Safety
/// `url` is storage that `URL__fromString` filled.
unsafe fn get<'a>(url: *const c_void) -> &'a url::Url {
    // SAFETY: the contract of this function.
    unsafe { &**url.cast::<Storage>() }
}

#[unsafe(no_mangle)]
unsafe extern "C" fn URL__deinit(url: *mut c_void) {
    // SAFETY: storage that `URL__fromString` filled, not used after this.
    drop(unsafe { Box::from_raw(*url.cast::<Storage>()) });
}

/// Without the `:`.
#[unsafe(no_mangle)]
unsafe extern "C" fn URL__protocol(url: *const c_void) -> EncodedSlice<'static> {
    // SAFETY: the caller's URL, which outlives what it borrows.
    EncodedSlice::latin1(unsafe { get(url) }.scheme().as_bytes())
}

#[unsafe(no_mangle)]
unsafe extern "C" fn URL__pathname(url: *const c_void) -> EncodedSlice<'static> {
    // SAFETY: as above.
    EncodedSlice::latin1(unsafe { get(url) }.path().as_bytes())
}

/// Without the `#`.
#[unsafe(no_mangle)]
unsafe extern "C" fn URL__fragmentIdentifier(url: *const c_void) -> EncodedSlice<'static> {
    // SAFETY: as above.
    EncodedSlice::latin1(unsafe { get(url) }.fragment().unwrap_or("").as_bytes())
}

/// The host WITH its port (`hostAndPort()`), unlike JS's `hostname`.
#[unsafe(no_mangle)]
unsafe extern "C" fn URL__hostname(url: *const c_void) -> BunString {
    // SAFETY: as above.
    let url = unsafe { get(url) };
    let host = url.host_str().unwrap_or("");
    match url.port() {
        Some(port) => BunString::clone_utf8(format!("{host}:{port}").as_bytes()),
        None => BunString::clone_utf8(host.as_bytes()),
    }
}

// ── dates ──────────────────────────────────────────────────────────────────

/// `TimeClip(ParseDateTimeString(..))` with a date-time without a zone read as UTC: ms since the epoch, or NaN.
#[unsafe(no_mangle)]
unsafe extern "C" fn Bun__parseDateTimeString(string: *const u8, length: usize) -> f64 {
    // SAFETY: the caller's slice.
    let text = unsafe { slice(string, length) };
    let ms = parse_iso(text).or_else(|| parse_legacy(text)).unwrap_or(f64::NAN);
    if ms.abs() <= 8.64e15 { ms.trunc() + 0.0 } else { f64::NAN }
}

fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (month + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

fn days_in_month(year: i64, month: i64) -> i64 {
    match month {
        2 if (year % 4 == 0 && year % 100 != 0) || year % 400 == 0 => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

struct Cursor<'a> {
    text: &'a [u8],
    at: usize,
}

impl Cursor<'_> {
    fn peek(&self) -> Option<u8> {
        self.text.get(self.at).copied()
    }
    fn eat(&mut self, byte: u8) -> bool {
        if self.peek() == Some(byte) {
            self.at += 1;
            true
        } else {
            false
        }
    }
    fn digits(&mut self, count: usize) -> Option<i64> {
        let digits = self.text.get(self.at..self.at + count)?;
        if !digits.iter().all(u8::is_ascii_digit) {
            return None;
        }
        self.at += count;
        Some(digits.iter().fold(0, |n, d| n * 10 + i64::from(d - b'0')))
    }
    fn done(&self) -> bool {
        self.at == self.text.len()
    }
}

fn time_ms(hour: i64, minute: i64, second: i64, ms: i64) -> Option<i64> {
    let valid = (hour < 24 && minute < 60 && second < 60) || (hour == 24 && minute == 0 && second == 0 && ms == 0);
    valid.then_some(((hour * 60 + minute) * 60 + second) * 1000 + ms)
}

/// ECMA-262's Date Time String Format: `[+-YY]YYYY[-MM[-DD]][THH:mm[:ss[.sss]][Z|±HH:mm]]`.
fn parse_iso(text: &[u8]) -> Option<f64> {
    let mut c = Cursor { text, at: 0 };
    let year = match c.peek()? {
        sign @ (b'+' | b'-') => {
            c.at += 1;
            let y = c.digits(6)?;
            if sign == b'-' && y == 0 {
                return None;
            }
            if sign == b'-' { -y } else { y }
        }
        _ => c.digits(4)?,
    };
    let (mut month, mut day) = (1, 1);
    if c.eat(b'-') {
        month = c.digits(2)?;
        if c.eat(b'-') {
            day = c.digits(2)?;
        }
    }
    if !(1..=12).contains(&month) || day < 1 || day > days_in_month(year, month) {
        return None;
    }
    let mut ms_of_day = 0;
    let mut offset_minutes = 0;
    if c.eat(b'T') || c.eat(b't') || c.eat(b' ') {
        let hour = c.digits(2)?;
        if !c.eat(b':') {
            return None;
        }
        let minute = c.digits(2)?;
        let (mut second, mut ms) = (0, 0);
        if c.eat(b':') {
            second = c.digits(2)?;
            if c.eat(b'.') || c.eat(b',') {
                let start = c.at;
                while c.peek().is_some_and(|b| b.is_ascii_digit()) {
                    c.at += 1;
                }
                let fraction = &text[start..c.at];
                if fraction.is_empty() {
                    return None;
                }
                ms = fraction.iter().take(3).fold(0, |n, d| n * 10 + i64::from(d - b'0'))
                    * 10_i64.pow(3 - fraction.len().min(3) as u32);
            }
        }
        ms_of_day = time_ms(hour, minute, second, ms)?;
        match c.peek() {
            Some(b'Z' | b'z') => c.at += 1,
            Some(sign @ (b'+' | b'-')) => {
                c.at += 1;
                let hours = c.digits(2)?;
                c.eat(b':');
                let minutes = c.digits(2)?;
                if hours > 23 || minutes > 59 {
                    return None;
                }
                offset_minutes = (hours * 60 + minutes) * if sign == b'+' { 1 } else { -1 };
            }
            _ => {}
        }
    }
    if !c.done() {
        return None;
    }
    let days = days_from_civil(year, month, day);
    Some((days * 86_400_000 + ms_of_day - offset_minutes * 60_000) as f64)
}

const MONTHS: [&[u8; 3]; 12] = [b"jan", b"feb", b"mar", b"apr", b"may", b"jun", b"jul", b"aug", b"sep", b"oct", b"nov", b"dec"];

/// The legacy forms that HTTP and RFC 2822 use: `[Day,] DD Mon YYYY HH:MM[:SS] [GMT|UTC|Z|±HHMM]`, RFC 850's
/// `Day, DD-Mon-YY HH:MM:SS GMT`, and asctime's `Day Mon D HH:MM:SS YYYY`. Without a zone the time is UTC.
fn parse_legacy(text: &[u8]) -> Option<f64> {
    let text = std::str::from_utf8(text).ok()?;
    let words: Vec<&str> = text.split(|c: char| c == ' ' || c == ',' || c == '-').filter(|w| !w.is_empty()).collect();
    let month_of = |word: &str| {
        let lower = word.to_ascii_lowercase();
        let prefix = lower.as_bytes().get(..3)?;
        MONTHS.iter().position(|m| m.as_slice() == prefix).map(|i| i as i64 + 1)
    };
    let (mut year, mut month, mut day, mut time, mut offset) = (None, None, None, None, 0i64);
    for word in words {
        if let Some(m) = month_of(word).filter(|_| word.bytes().all(|b| b.is_ascii_alphabetic())) {
            month = Some(m);
        } else if word.contains(':') {
            let parts: Vec<i64> = word.split(':').map(str::parse).collect::<Result<_, _>>().ok()?;
            let (h, m, s) = match parts.as_slice() {
                [h, m] => (*h, *m, 0),
                [h, m, s] => (*h, *m, *s),
                _ => return None,
            };
            time = Some(time_ms(h, m, s, 0)?);
        } else if let Some(zone) = word.strip_prefix('+').or_else(|| word.strip_prefix("GMT+")) {
            offset = zone_minutes(zone)?;
        } else if word.starts_with("GMT") || word == "UTC" || word == "UT" || word == "Z" {
            if let Some(rest) = word.strip_prefix("GMT").and_then(|r| r.strip_prefix('-')) {
                offset = -zone_minutes(rest)?;
            }
        } else if let Ok(n) = word.parse::<i64>() {
            if day.is_none() && n <= 31 && word.len() <= 2 && (month.is_some() || year.is_none()) {
                day = Some(n);
            } else if year.is_none() {
                year = Some(if word.len() <= 2 { if n < 50 { 2000 + n } else { 1900 + n } } else { n });
            } else {
                return None;
            }
        } else if word.bytes().all(|b| b.is_ascii_alphabetic()) {
            // A weekday, which says nothing that the date does not.
        } else {
            return None;
        }
    }
    let (year, month, day) = (year?, month?, day?);
    if day < 1 || day > days_in_month(year, month) {
        return None;
    }
    Some((days_from_civil(year, month, day) * 86_400_000 + time.unwrap_or(0) - offset * 60_000) as f64)
}

fn zone_minutes(zone: &str) -> Option<i64> {
    let digits: String = zone.chars().filter(|c| *c != ':').collect();
    if digits.len() != 4 || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let n: i64 = digits.parse().ok()?;
    Some(n / 100 * 60 + n % 100)
}

#[cfg(test)]
mod tests {
    fn parse(text: &str) -> f64 {
        // SAFETY: a slice.
        unsafe { super::Bun__parseDateTimeString(text.as_ptr(), text.len()) }
    }

    #[test]
    fn dates_parse_as_date_parse_does() {
        assert_eq!(parse("1970-01-01T00:00:00.000Z"), 0.0);
        assert_eq!(parse("2021-03-04T05:06:07.890Z"), 1614834367890.0);
        assert_eq!(parse("2021-03-04"), 1614816000000.0);
        assert_eq!(parse("2021-03-04T05:06:07+01:00"), 1614830767000.0);
        assert_eq!(parse("+275760-09-13T00:00:00.000Z"), 8.64e15);
        assert!(parse("+275760-09-13T00:00:00.001Z").is_nan());
        assert!(parse("2021-02-30").is_nan());
        assert!(parse("not a date").is_nan());
        assert_eq!(parse("Thu, 04 Mar 2021 05:06:07 GMT"), 1614834367000.0);
        assert_eq!(parse("Thursday, 04-Mar-21 05:06:07 GMT"), 1614834367000.0);
        assert_eq!(parse("Thu Mar  4 05:06:07 2021"), 1614834367000.0);
    }

    #[test]
    fn urls_parse_like_wtf() {
        let href = super::URL__getHref(&bun_core::String::static_("HTTP://Example.COM:80/a/../b?x#y"));
        assert_eq!(&*href.to_utf8(), b"http://example.com/b?x#y");
        let joined = super::URL__getHrefJoin(
            &bun_core::String::static_("http://127.0.0.1:1234/registry/"),
            &bun_core::String::static_("pkg/-/pkg-1.0.0.tgz"),
        );
        assert_eq!(&*joined.to_utf8(), b"http://127.0.0.1:1234/registry/pkg/-/pkg-1.0.0.tgz");
        assert!(super::URL__getHref(&bun_core::String::static_("not a url")).is_dead());
        let non_ascii = bun_core::String::clone_utf8("é".as_bytes());
        assert_eq!(&*non_ascii.to_utf8(), "é".as_bytes());
    }
}
