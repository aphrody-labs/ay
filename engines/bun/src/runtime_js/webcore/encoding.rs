//! Node's Buffer encodings over bytes and the code units of a string, without an engine: what
//! `src/runtime/webcore/encoding.rs` and `BufferVectorized::fill` do for the JavaScriptCore build, and the parts of
//! `JSBuffer.cpp` and `NodeBufferModule.cpp` (`byteLength`, `indexOf`, `transcode`) that only read and write bytes.

use bun_core::NodeEncoding as Encoding;
use bun_core::strings;

/// The code units of a string: Latin-1 when every unit fits in a byte, else UTF-16.
#[derive(Clone, Copy, Debug)]
pub enum Text<'a> {
    Latin1(&'a [u8]),
    Utf16(&'a [u16]),
}

impl Text<'_> {
    pub fn len(&self) -> usize {
        match self {
            Text::Latin1(units) => units.len(),
            Text::Utf16(units) => units.len(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    fn unit(&self, index: usize) -> u16 {
        match self {
            Text::Latin1(units) => u16::from(units[index]),
            Text::Utf16(units) => units[index],
        }
    }
}

/// `parseEnumerationFromView<BufferEncodingType>` (JSBufferEncodingType.cpp): the names of Node's encodings, in any
/// ASCII case. `buffer` is not one.
pub fn parse(name: Text<'_>) -> Option<Encoding> {
    if name.len() < 3 || name.len() > 9 {
        return None;
    }
    let mut lower = [0u8; 9];
    for (i, byte) in lower.iter_mut().take(name.len()).enumerate() {
        let unit = name.unit(i);
        *byte = u8::try_from(unit).ok()?.to_ascii_lowercase();
    }
    Some(match &lower[..name.len()] {
        b"utf8" | b"utf-8" => Encoding::Utf8,
        b"ucs2" | b"ucs-2" | b"utf16le" | b"utf-16le" => Encoding::Utf16le,
        b"latin1" | b"binary" => Encoding::Latin1,
        b"base64" => Encoding::Base64,
        b"base64url" => Encoding::Base64url,
        b"ascii" => Encoding::Ascii,
        b"hex" => Encoding::Hex,
        _ => return None,
    })
}

/// The name `encoding` is canonically known by (`BufferEncodingType`'s `convertEnumerationToJS`).
pub fn name(encoding: Encoding) -> &'static str {
    match encoding {
        Encoding::Utf8 => "utf8",
        Encoding::Ucs2 => "ucs2",
        Encoding::Utf16le => "utf16le",
        Encoding::Latin1 => "latin1",
        Encoding::Ascii => "ascii",
        Encoding::Base64 => "base64",
        Encoding::Base64url => "base64url",
        Encoding::Hex => "hex",
        Encoding::Buffer => "buffer",
    }
}

/// `Bun::byteLength`: how many bytes `Buffer.byteLength(text, encoding)` says the text is. For base64 it is Node's
/// estimate, from the length less its trailing `=`.
pub fn byte_length(text: Text<'_>, encoding: Encoding) -> usize {
    let length = text.len();
    if length == 0 {
        return 0;
    }
    match encoding {
        Encoding::Ucs2 | Encoding::Utf16le => length * 2,
        Encoding::Latin1 | Encoding::Ascii | Encoding::Buffer => length,
        Encoding::Base64 | Encoding::Base64url => {
            let mut length = length;
            if text.unit(length - 1) == u16::from(b'=') {
                length -= 1;
                if length > 1 && text.unit(length - 1) == u16::from(b'=') {
                    length -= 1;
                }
            }
            (length * 3) >> 2
        }
        Encoding::Hex => length >> 1,
        Encoding::Utf8 => match text {
            Text::Latin1(units) => strings::element_length_latin1_into_utf8(units),
            Text::Utf16(units) => strings::element_length_utf16_into_utf8(units),
        },
    }
}

/// The low byte of every code unit.
fn narrow(units: &[u16]) -> Vec<u8> {
    let mut out = vec![0u8; units.len()];
    strings::copy_u16_into_u8(&mut out, units);
    out
}

/// Hex pairs up to the first that is not one (`Buffer.from("…", "hex")`).
fn from_hex<Char: strings::HexChar>(input: &[Char]) -> Vec<u8> {
    let mut out = vec![0u8; input.len() / 2];
    let wrote = strings::decode_hex_to_bytes_truncate(&mut out, input);
    out.truncate(wrote);
    out
}

fn is_base64_space(byte: u8) -> bool {
    matches!(byte, b'\r' | b'\n' | b'\t' | b' ' | 0x0B)
}

/// Lenient base64 of either alphabet: what is not part of it is skipped, as Node does.
fn from_base64(input: &[u8], url_safe: bool) -> Vec<u8> {
    let start = input
        .iter()
        .position(|&byte| !is_base64_space(byte))
        .unwrap_or(input.len());
    let end = input
        .iter()
        .rposition(|&byte| !is_base64_space(byte))
        .map_or(start, |i| i + 1);
    let input = &input[start..end];
    if input.is_empty() {
        return Vec::new();
    }
    let mut out = vec![0u8; bun_base64::decode_lenient_len(input.len())];
    let wrote = bun_base64::decode_lenient(&mut out, input, url_safe);
    out.truncate(wrote);
    out
}

/// `constructFromEncoding` (JSBuffer.cpp): the bytes of `Buffer.from(text, encoding)`. A two-byte string is
/// decoded as base64 from the low byte of each unit, as Node's lenient decoder does.
pub fn encode(text: Text<'_>, encoding: Encoding) -> Vec<u8> {
    if text.is_empty() {
        return Vec::new();
    }
    match text {
        Text::Latin1(units) => match encoding {
            Encoding::Latin1 | Encoding::Ascii | Encoding::Buffer => units.to_vec(),
            Encoding::Utf8 => strings::allocate_latin1_into_utf8(units).unwrap_or_default(),
            Encoding::Ucs2 | Encoding::Utf16le => units
                .iter()
                .flat_map(|&unit| u16::from(unit).to_le_bytes())
                .collect(),
            Encoding::Hex => from_hex(units),
            Encoding::Base64 => from_base64(units, false),
            Encoding::Base64url => from_base64(units, true),
        },
        Text::Utf16(units) => match encoding {
            Encoding::Utf8 => strings::to_utf8_alloc_with_type(units),
            Encoding::Latin1 | Encoding::Ascii | Encoding::Buffer => narrow(units),
            Encoding::Ucs2 | Encoding::Utf16le => {
                units.iter().flat_map(|unit| unit.to_le_bytes()).collect()
            }
            Encoding::Hex => from_hex(units),
            Encoding::Base64 => from_base64(&narrow(units), false),
            Encoding::Base64url => from_base64(&narrow(units), true),
        },
    }
}

/// `write_u8`/`write_u16` of src/runtime/webcore/encoding.rs: encodes `text` into `to`, and how many bytes that is.
/// `partial` is `Buffer#fill`'s byte-level truncation: a unit or code point that only partly fits still writes its
/// leading bytes. Without it (`buf.write`) only whole ones are written. `ascii` writes as `latin1` does: the
/// bytes of the units as they are.
pub fn write(text: Text<'_>, to: &mut [u8], encoding: Encoding, partial: bool) -> usize {
    if text.is_empty() || to.is_empty() {
        return 0;
    }
    match text {
        Text::Latin1(input) => match encoding {
            Encoding::Buffer | Encoding::Latin1 | Encoding::Ascii => {
                let written = input.len().min(to.len());
                to[..written].copy_from_slice(&input[..written]);
                written
            }
            Encoding::Utf8 => {
                let result = strings::copy_latin1_into_utf8(to, input);
                let mut written = result.written;
                if partial && written < to.len() && result.read < input.len() {
                    to[written] = 0xC0 | (input[result.read] >> 6);
                    written += 1;
                }
                written
            }
            Encoding::Ucs2 | Encoding::Utf16le => {
                let units = input.len().min(to.len() / 2);
                for (pair, &unit) in to.chunks_exact_mut(2).zip(&input[..units]) {
                    pair.copy_from_slice(&u16::from(unit).to_le_bytes());
                }
                let mut written = units * 2;
                if partial && written < to.len() && written < input.len() * 2 {
                    to[written] = input[written / 2];
                    written += 1;
                }
                written
            }
            Encoding::Hex => strings::decode_hex_to_bytes_truncate(to, input),
            Encoding::Base64 => bun_base64::decode_lenient(to, input, false),
            Encoding::Base64url => bun_base64::decode_lenient(to, input, true),
        },
        Text::Utf16(input) => match encoding {
            Encoding::Utf8 => {
                if partial {
                    strings::copy_utf16_into_utf8_impl::<true>(to, input).written
                } else {
                    strings::copy_utf16_into_utf8_impl::<false>(to, input).written
                }
            }
            Encoding::Latin1 | Encoding::Ascii | Encoding::Buffer => {
                let written = input.len().min(to.len());
                strings::copy_u16_into_u8(&mut to[..written], &input[..written]);
                written
            }
            Encoding::Ucs2 | Encoding::Utf16le => {
                let available = (input.len() * 2).min(to.len());
                let written = if partial {
                    available
                } else {
                    available & !1
                };
                for (i, byte) in to[..written].iter_mut().enumerate() {
                    *byte = input[i / 2].to_le_bytes()[i % 2];
                }
                written
            }
            Encoding::Hex => strings::decode_hex_to_bytes_truncate(to, input),
            Encoding::Base64 => bun_base64::decode_lenient(to, &narrow(input), false),
            Encoding::Base64url => bun_base64::decode_lenient(to, &narrow(input), true),
        },
    }
}

/// `BufferVectorized::fill`: fills `buffer` with the encoding of `text`, repeated. `false` when `text` encodes to
/// no bytes at all (`Buffer.alloc(4, "zz", "hex")`), which Node rejects.
pub fn fill(buffer: &mut [u8], text: Text<'_>, encoding: Encoding) -> bool {
    if text.is_empty() {
        return true;
    }
    let written = write(text, buffer, encoding, true);
    if written == 0 {
        return buffer.is_empty();
    }
    repeat(buffer, written);
    true
}

/// Repeats the first `seed` bytes of `buffer` over the rest of it.
pub fn repeat(buffer: &mut [u8], seed: usize) {
    let mut length = seed;
    let mut offset = seed;
    while length > 0 && buffer.len() - offset >= length {
        buffer.copy_within(0..length, offset);
        offset += length;
        length *= 2;
    }
    let remaining = buffer.len() - offset;
    if remaining > 0 {
        buffer.copy_within(0..remaining, offset);
    }
}

/// A string `decode` makes: Latin-1 bytes or UTF-16 units.
#[derive(Debug, PartialEq, Eq)]
pub enum Decoded {
    Latin1(Vec<u8>),
    Utf16(Vec<u16>),
}

fn to_base64(bytes: &[u8], url_safe: bool) -> Vec<u8> {
    let length = if url_safe {
        bun_base64::url_safe_encode_len(bytes)
    } else {
        bun_base64::encode_len(bytes)
    };
    let mut out = vec![0u8; length];
    let wrote = if url_safe {
        bun_base64::encode_url_safe(&mut out, bytes)
    } else {
        bun_base64::encode(&mut out, bytes)
    };
    out.truncate(wrote);
    out
}

/// `jsBufferToStringFromBytes`: the string `buf.toString(encoding)` makes of `bytes`. UTF-8 has U+FFFD for each
/// maximal invalid subsequence, `ascii` clears the high bit, `utf16le` drops a trailing odd byte.
pub fn decode(bytes: &[u8], encoding: Encoding) -> Decoded {
    if bytes.is_empty() {
        return Decoded::Latin1(Vec::new());
    }
    match encoding {
        Encoding::Latin1 => Decoded::Latin1(bytes.to_vec()),
        Encoding::Ascii => {
            let mut out = vec![0u8; bytes.len()];
            strings::copy_latin1_into_ascii(&mut out, bytes);
            Decoded::Latin1(out)
        }
        Encoding::Ucs2 | Encoding::Utf16le => Decoded::Utf16(
            bytes
                .chunks_exact(2)
                .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
                .collect(),
        ),
        Encoding::Hex => {
            let mut out = vec![0u8; bytes.len() * 2];
            strings::encode_bytes_to_hex(&mut out, bytes);
            Decoded::Latin1(out)
        }
        Encoding::Base64 => Decoded::Latin1(to_base64(bytes, false)),
        Encoding::Base64url => Decoded::Latin1(to_base64(bytes, true)),
        Encoding::Utf8 | Encoding::Buffer => match strings::to_utf16_alloc(bytes, false, false) {
            Ok(Some(units)) => Decoded::Utf16(units),
            // All ASCII.
            Ok(None) => Decoded::Latin1(bytes.to_vec()),
            Err(_) => Decoded::Utf16(String::from_utf8_lossy(bytes).encode_utf16().collect()),
        },
    }
}

/// `indexOfOffset` of Node's node_buffer.cc.
fn index_of_offset(length: i64, offset: i64, needle_length: i64, forward: bool) -> i64 {
    if offset < 0 {
        if offset + length >= 0 {
            length + offset
        } else if forward || needle_length == 0 {
            0
        } else {
            -1
        }
    } else if offset + needle_length <= length {
        offset
    } else if needle_length == 0 {
        length
    } else if forward {
        -1
    } else {
        length - 1
    }
}

/// Where a match of `needle` at an even offset of `haystack` starts: a UTF-16 search compares whole units.
fn find_even(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    let mut from = 0;
    while from + needle.len() <= haystack.len() {
        let at = from + strings::index_of(&haystack[from..], needle)?;
        if at % 2 == 0 {
            return Some(at);
        }
        from = at + 1;
    }
    None
}

fn rfind_even(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    let mut end = haystack.len();
    while end >= needle.len() {
        let at = strings::last_index_of(&haystack[..end], needle)?;
        if at % 2 == 0 {
            return Some(at);
        }
        end = at + needle.len() - 1;
    }
    None
}

/// `buf.indexOf(needle, byteOffset, end)` and `lastIndexOf` once the needle is bytes, from `computeIndexOfRange`
/// and the searches of JSBuffer.cpp: `byte_offset` is already clamped to the int32 range (NaN is the start or the
/// end), `end` is the exclusive upper bound of the search. `utf16` searches whole 16-bit units.
pub fn index_of(
    haystack: &[u8],
    needle: &[u8],
    byte_offset: f64,
    end: f64,
    last: bool,
    utf16: bool,
) -> i64 {
    let forward = !last;
    let mut search_end = end.max(0.0).min(haystack.len() as f64) as usize;
    if utf16 {
        search_end &= !1;
    }
    let offset = index_of_offset(
        haystack.len() as i64,
        byte_offset as i64,
        needle.len() as i64,
        forward,
    );
    if needle.is_empty() {
        return offset.min(search_end as i64);
    }
    if haystack.is_empty() || offset <= -1 {
        return -1;
    }
    let mut offset = offset as usize;
    if offset >= search_end {
        if forward || search_end == 0 {
            return -1;
        }
        offset = search_end - 1;
    }
    if (forward && needle.len() + offset > search_end) || needle.len() > search_end {
        return -1;
    }
    let haystack = &haystack[..search_end];
    if utf16 {
        if haystack.len() == 1 || needle.len() == 1 {
            return -1;
        }
        let needle = &needle[..needle.len() & !1];
        let offset = offset & !1;
        let found = if forward {
            find_even(&haystack[offset..], needle).map(|at| at + offset)
        } else {
            let length = haystack.len().min(offset + needle.len());
            rfind_even(&haystack[..length], needle)
        };
        return found.map_or(-1, |at| at as i64);
    }
    let found = if forward {
        if needle.len() == 1 {
            strings::index_of_char_usize(&haystack[offset..], needle[0])
        } else {
            strings::index_of(&haystack[offset..], needle)
        }
        .map(|at| at + offset)
    } else {
        let length = haystack.len().min(offset + needle.len());
        if length < needle.len() {
            None
        } else if needle.len() == 1 {
            strings::last_index_of_char(&haystack[..length], needle[0])
        } else {
            strings::last_index_of(&haystack[..length], needle)
        }
    };
    found.map_or(-1, |at| at as i64)
}

/// `memcmp` of the two, normalized to -1, 0 or 1, the shorter first where one is a prefix of the other.
pub fn compare(a: &[u8], b: &[u8]) -> i32 {
    match a.cmp(b) {
        core::cmp::Ordering::Less => -1,
        core::cmp::Ordering::Equal => 0,
        core::cmp::Ordering::Greater => 1,
    }
}

/// Reverses the byte order of each `size`-byte element (`swap16`, `swap32`, `swap64`).
pub fn swap(bytes: &mut [u8], size: usize) {
    for element in bytes.chunks_exact_mut(size) {
        element.reverse();
    }
}

/// The encodings `buffer.transcode` converts between (Node's `SupportedEncoding`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TranscodeEncoding {
    Ascii,
    Latin1,
    Ucs2,
    Utf8,
}

impl TranscodeEncoding {
    /// Node's `normalizeEncoding`, then what transcode supports; `None` is `U_ILLEGAL_ARGUMENT_ERROR`.
    pub fn parse(name: &str) -> Option<TranscodeEncoding> {
        match name.to_ascii_lowercase().as_str() {
            "" | "utf8" | "utf-8" => Some(TranscodeEncoding::Utf8),
            "ucs2" | "ucs-2" | "utf16le" | "utf-16le" => Some(TranscodeEncoding::Ucs2),
            "latin1" | "binary" => Some(TranscodeEncoding::Latin1),
            "ascii" => Some(TranscodeEncoding::Ascii),
            _ => None,
        }
    }
}

/// Why `transcode` failed: the ICU status Node reports, as `code`, and its number, as `errno`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TranscodeError {
    IllegalArgument,
    InvalidChar,
}

impl TranscodeError {
    pub fn name(self) -> &'static str {
        match self {
            TranscodeError::IllegalArgument => "U_ILLEGAL_ARGUMENT_ERROR",
            TranscodeError::InvalidChar => "U_INVALID_CHAR_FOUND",
        }
    }

    pub fn errno(self) -> i32 {
        match self {
            TranscodeError::IllegalArgument => 1,
            TranscodeError::InvalidChar => 10,
        }
    }
}

fn utf16le_units(bytes: &[u8]) -> Vec<u16> {
    bytes
        .chunks_exact(2)
        .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
        .collect()
}

fn units_to_bytes(units: &[u16]) -> Vec<u8> {
    units.iter().flat_map(|unit| unit.to_le_bytes()).collect()
}

/// Decodes to well-formed UTF-16, as ICU's pivot does: U+FFFD for what does not decode.
fn transcode_to_utf16(input: &[u8], from: TranscodeEncoding, replace_odd_byte: bool) -> Vec<u16> {
    match from {
        TranscodeEncoding::Latin1 => input.iter().map(|&byte| u16::from(byte)).collect(),
        TranscodeEncoding::Ascii => input
            .iter()
            .map(|&byte| if byte > 0x7F { 0xFFFD } else { u16::from(byte) })
            .collect(),
        TranscodeEncoding::Utf8 => String::from_utf8_lossy(input).encode_utf16().collect(),
        TranscodeEncoding::Ucs2 => {
            let mut units: Vec<u16> = char::decode_utf16(utf16le_units(input))
                .map(|unit| unit.unwrap_or(char::REPLACEMENT_CHARACTER))
                .collect::<String>()
                .encode_utf16()
                .collect();
            if replace_odd_byte && input.len() % 2 == 1 {
                units.push(0xFFFD);
            }
            units
        }
    }
}

/// Well-formed UTF-16 to a single-byte encoding: what is above `max` becomes `?`, one for a surrogate pair.
fn transcode_narrow(units: &[u16], max: u16) -> Vec<u8> {
    units
        .iter()
        .filter(|&&unit| !(0xDC00..=0xDFFF).contains(&unit))
        .map(|&unit| if unit <= max { unit as u8 } else { b'?' })
        .collect()
}

/// `buffer.transcode(source, from, to)` once its encodings are known (`jsBufferTranscode`): an empty source is an
/// empty result, whatever the encodings.
pub fn transcode(
    input: &[u8],
    from: Option<TranscodeEncoding>,
    to: Option<TranscodeEncoding>,
) -> Result<Vec<u8>, TranscodeError> {
    if input.is_empty() {
        return Ok(Vec::new());
    }
    let (Some(from), Some(to)) = (from, to) else {
        return Err(TranscodeError::IllegalArgument);
    };
    use TranscodeEncoding::{Ascii, Latin1, Ucs2, Utf8};
    match (from, to) {
        (Ascii | Latin1, Ucs2) => Ok(input
            .iter()
            .flat_map(|&byte| u16::from(byte).to_le_bytes())
            .collect()),
        (Utf8, Ucs2) => match core::str::from_utf8(input) {
            Ok(text) => Ok(text
                .encode_utf16()
                .flat_map(|unit| unit.to_le_bytes())
                .collect()),
            Err(_) => Err(TranscodeError::InvalidChar),
        },
        (Ucs2, Utf8) => {
            let units = utf16le_units(input);
            if units.is_empty() {
                return Err(TranscodeError::InvalidChar);
            }
            match String::from_utf16(&units) {
                Ok(text) => Ok(text.into_bytes()),
                Err(_) => Err(TranscodeError::InvalidChar),
            }
        }
        (from, to) => {
            let units = transcode_to_utf16(input, from, to == Ucs2);
            Ok(match to {
                Latin1 => transcode_narrow(&units, 0xFF),
                Ascii => transcode_narrow(&units, 0x7F),
                Ucs2 => units_to_bytes(&units),
                Utf8 => String::from_utf16_lossy(&units).into_bytes(),
            })
        }
    }
}

/// `isUtf8`.
pub fn is_utf8(bytes: &[u8]) -> bool {
    bun_simdutf_sys::simdutf::validate::utf8(bytes)
}

/// `isAscii`.
pub fn is_ascii(bytes: &[u8]) -> bool {
    strings::is_all_ascii(bytes)
}
