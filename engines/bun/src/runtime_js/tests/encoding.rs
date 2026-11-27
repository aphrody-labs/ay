//! The encoders of `Buffer`, against what Node (and Bun on JavaScriptCore) answers for the same calls.

use bun_core::NodeEncoding as Encoding;
use bun_runtime_js::webcore::encoding::{
    self, Decoded, Text, TranscodeEncoding, TranscodeError, byte_length, decode, encode, fill,
    index_of, transcode, write,
};

fn latin1(text: &str) -> Text<'_> {
    Text::Latin1(text.as_bytes())
}

fn utf16(text: &str) -> Vec<u16> {
    text.encode_utf16().collect()
}

#[test]
fn parses_encoding_names() {
    for (name, expected) in [
        ("utf8", Some(Encoding::Utf8)),
        ("UTF-8", Some(Encoding::Utf8)),
        ("ucs2", Some(Encoding::Utf16le)),
        ("UCS-2", Some(Encoding::Utf16le)),
        ("utf16le", Some(Encoding::Utf16le)),
        ("utf-16le", Some(Encoding::Utf16le)),
        ("latin1", Some(Encoding::Latin1)),
        ("binary", Some(Encoding::Latin1)),
        ("Base64", Some(Encoding::Base64)),
        ("base64url", Some(Encoding::Base64url)),
        ("ascii", Some(Encoding::Ascii)),
        ("HEX", Some(Encoding::Hex)),
        ("buffer", None),
        ("utf16-le", None),
        ("u8", None),
        ("", None),
        ("utf32", None),
    ] {
        assert_eq!(encoding::parse(latin1(name)), expected, "{name}");
    }
    let wide = utf16("utf\u{0138}");
    assert_eq!(encoding::parse(Text::Utf16(&wide)), None);
}

#[test]
fn byte_lengths() {
    assert_eq!(byte_length(Text::Latin1(b"h\xE9llo"), Encoding::Utf8), 6);
    assert_eq!(byte_length(Text::Latin1(&[0xE9]), Encoding::Utf8), 2);
    let wide = utf16("€😀");
    assert_eq!(byte_length(Text::Utf16(&wide), Encoding::Utf8), 7);
    assert_eq!(byte_length(Text::Utf16(&wide), Encoding::Utf16le), 6);
    assert_eq!(byte_length(latin1("abcd"), Encoding::Latin1), 4);
    assert_eq!(byte_length(latin1("aGVsbG8="), Encoding::Base64), 5);
    assert_eq!(byte_length(latin1("aGVsbA=="), Encoding::Base64), 4);
    assert_eq!(byte_length(latin1("abc"), Encoding::Hex), 1);
    assert_eq!(byte_length(latin1(""), Encoding::Base64), 0);
}

#[test]
fn encodes_strings() {
    assert_eq!(encode(latin1("hi"), Encoding::Utf8), b"hi");
    assert_eq!(encode(Text::Latin1(&[0xE9]), Encoding::Utf8), [0xC3, 0xA9]);
    assert_eq!(encode(Text::Latin1(&[0xE9]), Encoding::Latin1), [0xE9]);
    assert_eq!(encode(latin1("ab"), Encoding::Utf16le), [b'a', 0, b'b', 0]);
    assert_eq!(encode(latin1("0aFfzz11"), Encoding::Hex), [0x0A, 0xFF]);
    assert_eq!(encode(latin1("a"), Encoding::Hex), b"");
    assert_eq!(encode(latin1(" aGVsbG8 \n"), Encoding::Base64), b"hello");
    assert_eq!(encode(latin1("aGVsbG8"), Encoding::Base64), b"hello");
    assert_eq!(encode(latin1("_-8"), Encoding::Base64url), [0xFF, 0xEF]);
    assert_eq!(encode(latin1("_-8"), Encoding::Base64), [0xFF, 0xEF]);
    // A lone surrogate is U+FFFD in UTF-8.
    let lone = [0xD800u16];
    assert_eq!(encode(Text::Utf16(&lone), Encoding::Utf8), [0xEF, 0xBF, 0xBD]);
    let wide = utf16("€");
    assert_eq!(encode(Text::Utf16(&wide), Encoding::Latin1), [0xAC]);
    assert_eq!(encode(Text::Utf16(&wide), Encoding::Utf16le), [0xAC, 0x20]);
}

#[test]
fn writes_whole_characters() {
    let mut to = [0u8; 4];
    let wide = utf16("a€");
    assert_eq!(write(Text::Utf16(&wide), &mut to[..3], Encoding::Utf8, false), 1);
    assert_eq!(write(Text::Utf16(&wide), &mut to, Encoding::Utf8, false), 4);
    assert_eq!(to, [b'a', 0xE2, 0x82, 0xAC]);
    let mut to = [0u8; 3];
    assert_eq!(write(latin1("ab"), &mut to, Encoding::Utf16le, false), 2);
    assert_eq!(write(Text::Utf16(&wide), &mut to, Encoding::Utf16le, false), 2);
    let mut to = [0u8; 2];
    assert_eq!(write(Text::Latin1(&[0xE9, 0xE9]), &mut to, Encoding::Utf8, false), 2);
    assert_eq!(to, [0xC3, 0xA9]);
    assert_eq!(write(latin1("abcd"), &mut to, Encoding::Latin1, false), 2);
}

#[test]
fn fills_with_partial_characters() {
    let mut buffer = [0u8; 5];
    assert!(fill(&mut buffer, latin1("ab"), Encoding::Utf8));
    assert_eq!(&buffer, b"ababa");
    let mut buffer = [0u8; 3];
    assert!(fill(&mut buffer, Text::Latin1(&[0xE9]), Encoding::Utf8));
    assert_eq!(buffer, [0xC3, 0xA9, 0xC3]);
    let mut buffer = [0u8; 3];
    assert!(fill(&mut buffer, latin1("a"), Encoding::Utf16le));
    assert_eq!(buffer, [b'a', 0, b'a']);
    let mut buffer = [0u8; 4];
    assert!(!fill(&mut buffer, latin1("zz"), Encoding::Hex));
    assert!(fill(&mut buffer, latin1("1"), Encoding::Hex) == false);
    assert!(fill(&mut buffer, latin1("abc"), Encoding::Hex));
    assert_eq!(buffer, [0xAB; 4]);
}

#[test]
fn decodes_bytes() {
    assert_eq!(decode(b"hi", Encoding::Utf8), Decoded::Latin1(b"hi".to_vec()));
    assert_eq!(
        decode(&[0xC3, 0xA9, 0xFF, 0xE2, 0x82], Encoding::Utf8),
        Decoded::Utf16(utf16("é\u{FFFD}\u{FFFD}"))
    );
    assert_eq!(decode(&[0xE9, 0x41], Encoding::Ascii), Decoded::Latin1(vec![0x69, 0x41]));
    assert_eq!(decode(&[0xE9], Encoding::Latin1), Decoded::Latin1(vec![0xE9]));
    assert_eq!(decode(&[0x61, 0, 0x62], Encoding::Utf16le), Decoded::Utf16(utf16("a")));
    assert_eq!(decode(&[0x0A, 0xFF], Encoding::Hex), Decoded::Latin1(b"0aff".to_vec()));
    assert_eq!(decode(b"hello", Encoding::Base64), Decoded::Latin1(b"aGVsbG8=".to_vec()));
    assert_eq!(decode(&[0xFF, 0xEF], Encoding::Base64url), Decoded::Latin1(b"_-8".to_vec()));
}

#[test]
fn finds_needles() {
    let hay = b"abcabc";
    assert_eq!(index_of(hay, b"bc", 0.0, f64::INFINITY, false, false), 1);
    assert_eq!(index_of(hay, b"bc", 2.0, f64::INFINITY, false, false), 4);
    assert_eq!(index_of(hay, b"bc", -2.0, f64::INFINITY, false, false), 4);
    assert_eq!(index_of(hay, b"bc", 6.0, f64::INFINITY, true, false), 4);
    assert_eq!(index_of(hay, b"bc", 3.0, f64::INFINITY, true, false), 1);
    assert_eq!(index_of(hay, b"bc", -100.0, f64::INFINITY, true, false), -1);
    assert_eq!(index_of(hay, b"c", 0.0, 2.0, false, false), -1);
    assert_eq!(index_of(hay, b"", 3.0, f64::INFINITY, false, false), 3);
    assert_eq!(index_of(hay, b"", 10.0, f64::INFINITY, false, false), 6);
    assert_eq!(index_of(b"", b"", 0.0, f64::INFINITY, false, false), 0);
    assert_eq!(index_of(b"", b"a", 0.0, f64::INFINITY, false, false), -1);
    // UTF-16 matches start at even offsets only.
    let units = [b'a', b'b', 0, b'x', b'b', 0];
    assert_eq!(index_of(&units, &[b'b', 0], 0.0, f64::INFINITY, false, true), 4);
    assert_eq!(index_of(&units, &[b'b', 0], 6.0, f64::INFINITY, true, true), 4);
    assert_eq!(index_of(&units, &[b'b', 0], 3.0, f64::INFINITY, true, true), -1);
}

#[test]
fn transcodes() {
    use TranscodeEncoding::{Ascii, Latin1, Ucs2, Utf8};
    assert_eq!(transcode("€".as_bytes(), Some(Utf8), Some(Ucs2)), Ok(vec![0xAC, 0x20]));
    assert_eq!(transcode(&[0xFF], Some(Utf8), Some(Ucs2)), Err(TranscodeError::InvalidChar));
    assert_eq!(transcode(&[0xAC, 0x20], Some(Ucs2), Some(Utf8)), Ok("€".as_bytes().to_vec()));
    assert_eq!(transcode(&[0x00, 0xD8], Some(Ucs2), Some(Utf8)), Err(TranscodeError::InvalidChar));
    assert_eq!(transcode(&[0x61], Some(Ucs2), Some(Utf8)), Err(TranscodeError::InvalidChar));
    assert_eq!(transcode("€a".as_bytes(), Some(Utf8), Some(Latin1)), Ok(b"?a".to_vec()));
    assert_eq!(transcode("😀".as_bytes(), Some(Utf8), Some(Ascii)), Ok(b"?".to_vec()));
    assert_eq!(transcode(&[0xE9], Some(Latin1), Some(Utf8)), Ok(vec![0xC3, 0xA9]));
    assert_eq!(transcode(&[0xE9], Some(Ascii), Some(Utf8)), Ok("\u{FFFD}".as_bytes().to_vec()));
    assert_eq!(transcode(&[0x61, 0, 0x62], Some(Ucs2), Some(Ucs2)), Ok(vec![0x61, 0, 0xFD, 0xFF]));
    assert_eq!(transcode(b"a", None, Some(Utf8)), Err(TranscodeError::IllegalArgument));
    assert_eq!(transcode(b"", None, None), Ok(Vec::new()));
    assert_eq!(TranscodeEncoding::parse("BINARY"), Some(Latin1));
    assert_eq!(TranscodeEncoding::parse("base64"), None);
}

#[test]
fn compares_and_swaps() {
    assert_eq!(encoding::compare(b"ab", b"abc"), -1);
    assert_eq!(encoding::compare(b"b", b"abc"), 1);
    assert_eq!(encoding::compare(b"", b""), 0);
    let mut bytes = [1, 2, 3, 4, 5, 6, 7, 8];
    encoding::swap(&mut bytes, 2);
    assert_eq!(bytes, [2, 1, 4, 3, 6, 5, 8, 7]);
    encoding::swap(&mut bytes, 8);
    assert_eq!(bytes, [7, 8, 5, 6, 3, 4, 1, 2]);
    assert!(encoding::is_utf8("é".as_bytes()) && !encoding::is_utf8(&[0xFF]));
    assert!(encoding::is_ascii(b"abc") && !encoding::is_ascii(&[0x80]));
}
