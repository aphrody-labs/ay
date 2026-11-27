//! The native half of `Buffer` on Boa: what JSBuffer.cpp and NodeBufferModule.cpp do with bytes for the
//! JavaScriptCore build, as functions of views and strings that packages/bun-boa/src/js/buffer.ts has already
//! validated, the way Node's lib/buffer.js calls its bindings. Encodings are `bun_core::NodeEncoding` numbers.

use bun_core::NodeEncoding as Encoding;
use bun_js::{ArrayBuffer, CallFrame, JSFunction, JSGlobalObject, JSHostFn, JSObject, JSValue, JsResult, StringUnits};

use crate::webcore::encoding::{self, Decoded, Text, TranscodeEncoding};

pub(crate) fn text(units: StringUnits<'_>) -> Text<'_> {
    match units {
        StringUnits::Latin1(units) => Text::Latin1(units),
        StringUnits::Utf16(units) => Text::Utf16(units),
    }
}

fn encoding_of(value: &JSValue) -> Encoding {
    match value.as_number() as u8 {
        1 => Encoding::Ucs2,
        2 => Encoding::Utf16le,
        3 => Encoding::Latin1,
        4 => Encoding::Ascii,
        5 => Encoding::Base64,
        6 => Encoding::Base64url,
        7 => Encoding::Hex,
        8 => Encoding::Buffer,
        _ => Encoding::Utf8,
    }
}

/// A non-negative integer argument, clamped to `max`.
fn index(value: &JSValue, max: usize) -> usize {
    let number = value.as_number();
    if number.is_nan() || number <= 0.0 {
        0
    } else {
        (number as usize).min(max)
    }
}

/// `bytes[start..end]`, empty where the bytes are gone: a view of a detached buffer has none.
fn part(bytes: &[u8], start: usize, end: usize) -> &[u8] {
    bytes.get(start..end).unwrap_or_default()
}

fn part_mut(bytes: &mut [u8], start: usize, end: usize) -> &mut [u8] {
    bytes.get_mut(start..end).unwrap_or_default()
}

fn view(global: &JSGlobalObject, value: &JSValue) -> JsResult<ArrayBuffer> {
    match value.as_array_buffer(global)? {
        Some(view) => Ok(view),
        None => Err(global.throw_type_error(format_args!("Expected ArrayBufferView"))),
    }
}

fn string<R>(global: &JSGlobalObject, value: &JSValue, f: impl FnOnce(Text<'_>) -> R) -> JsResult<R> {
    match value.with_string_units(|units| f(text(units))) {
        Some(result) => Ok(result),
        None => Err(global.throw_type_error(format_args!("argument must be a string"))),
    }
}

fn to_js(decoded: Decoded) -> JSValue {
    match decoded {
        Decoded::Latin1(units) => JSValue::from_latin1(&units),
        Decoded::Utf16(units) => JSValue::from_utf16(&units),
    }
}

/// `parseEncoding(name)`: the encoding's number, -1 for what is not one.
fn parse_encoding(_: &JSGlobalObject, frame: &CallFrame<'_>) -> JsResult<JSValue> {
    let parsed = frame
        .argument(0)
        .with_string_units(|units| encoding::parse(text(units)))
        .flatten();
    Ok(JSValue::js_number(parsed.map_or(-1.0, |encoding| f64::from(encoding as u8))))
}

/// `byteLength(string, encoding)`.
fn byte_length(global: &JSGlobalObject, frame: &CallFrame<'_>) -> JsResult<JSValue> {
    let encoding = encoding_of(&frame.argument(1));
    let length = string(global, &frame.argument(0), |text| encoding::byte_length(text, encoding))?;
    Ok(JSValue::js_number(length as f64))
}

/// `encode(string, encoding)`: a new `Uint8Array` of the string's bytes.
fn encode(global: &JSGlobalObject, frame: &CallFrame<'_>) -> JsResult<JSValue> {
    let encoding = encoding_of(&frame.argument(1));
    let bytes = string(global, &frame.argument(0), |text| encoding::encode(text, encoding))?;
    JSValue::create_uint8_array(global, &bytes)
}

/// `write(view, string, offset, length, encoding)`: how many bytes of the string fit from `offset`, at most
/// `length`, written whole.
fn write(global: &JSGlobalObject, frame: &CallFrame<'_>) -> JsResult<JSValue> {
    let target = view(global, &frame.argument(0))?;
    let source = frame.argument(1);
    let offset = index(&frame.argument(2), target.byte_len());
    let length = index(&frame.argument(3), target.byte_len() - offset);
    let encoding = encoding_of(&frame.argument(4));
    let written = source.with_string_units(|units| {
        target.with_bytes_mut(|bytes| {
            encoding::write(text(units), part_mut(bytes, offset, offset + length), encoding, false)
        })
    });
    match written {
        Some(written) => Ok(JSValue::js_number(written as f64)),
        None => Err(global.throw_type_error(format_args!("argument must be a string"))),
    }
}

/// `decode(view, start, end, encoding)`: the string of the bytes from `start` to `end`.
fn decode(global: &JSGlobalObject, frame: &CallFrame<'_>) -> JsResult<JSValue> {
    let source = view(global, &frame.argument(0))?;
    let length = source.byte_len();
    let start = index(&frame.argument(1), length);
    let end = index(&frame.argument(2), length).max(start);
    let encoding = encoding_of(&frame.argument(3));
    Ok(to_js(source.with_bytes(|bytes| encoding::decode(part(bytes, start, end), encoding))))
}

/// `fill(view, start, end, value, encoding)`: `value` is a string, a view or a byte. `false` when a string has no
/// bytes in the encoding.
fn fill(global: &JSGlobalObject, frame: &CallFrame<'_>) -> JsResult<JSValue> {
    let target = view(global, &frame.argument(0))?;
    let start = index(&frame.argument(1), target.byte_len());
    let end = index(&frame.argument(2), target.byte_len()).max(start);
    let value = frame.argument(3);
    let encoding = match encoding_of(&frame.argument(4)) {
        Encoding::Ascii => Encoding::Latin1,
        encoding => encoding,
    };
    if let Some(pattern) = value.as_array_buffer(global)? {
        let pattern = pattern.with_bytes(<[u8]>::to_vec);
        target.with_bytes_mut(|bytes| {
            let bytes = part_mut(bytes, start, end);
            let seed = pattern.len().min(bytes.len());
            bytes[..seed].copy_from_slice(&pattern[..seed]);
            encoding::repeat(bytes, seed);
        });
        return Ok(JSValue::js_boolean(true));
    }
    if value.is_string() {
        let filled = value.with_string_units(|units| {
            target.with_bytes_mut(|bytes| encoding::fill(part_mut(bytes, start, end), text(units), encoding))
        });
        return Ok(JSValue::js_boolean(filled.unwrap_or(false)));
    }
    let byte = value.as_number() as i64 as u8;
    target.with_bytes_mut(|bytes| part_mut(bytes, start, end).fill(byte));
    Ok(JSValue::js_boolean(true))
}

/// `indexOf(view, needle, byteOffset, end, last, encoding)`: `needle` is a byte, a string in `encoding` or a view.
fn index_of(global: &JSGlobalObject, frame: &CallFrame<'_>) -> JsResult<JSValue> {
    let haystack = view(global, &frame.argument(0))?;
    let needle = frame.argument(1);
    let byte_offset = frame.argument(2).as_number();
    let end = frame.argument(3).as_number();
    let last = frame.argument(4).to_boolean();
    let encoding = encoding_of(&frame.argument(5));
    let utf16 = matches!(encoding, Encoding::Ucs2 | Encoding::Utf16le);
    let found = if let Some(number) = needle.get_number() {
        let byte = [number as i64 as u8];
        haystack.with_bytes(|bytes| encoding::index_of(bytes, &byte, byte_offset, end, last, false))
    } else if needle.is_string() {
        let needle = string(global, &needle, |text| encoding::encode(text, encoding))?;
        haystack.with_bytes(|bytes| {
            // Node's IndexOfString searches an even number of bytes for UTF-16.
            let bytes = if utf16 { &bytes[..bytes.len() & !1] } else { bytes };
            encoding::index_of(bytes, &needle, byte_offset, end, last, utf16)
        })
    } else {
        let needle = view(global, &needle)?.with_bytes(<[u8]>::to_vec);
        haystack.with_bytes(|bytes| encoding::index_of(bytes, &needle, byte_offset, end, last, utf16))
    };
    Ok(JSValue::js_number(found as f64))
}

/// `compare(a, aStart, aEnd, b, bStart, bEnd)`.
fn compare(global: &JSGlobalObject, frame: &CallFrame<'_>) -> JsResult<JSValue> {
    let a = view(global, &frame.argument(0))?;
    let b = view(global, &frame.argument(3))?;
    let a_end = index(&frame.argument(2), a.byte_len());
    let a_start = index(&frame.argument(1), a_end);
    let b_end = index(&frame.argument(5), b.byte_len());
    let b_start = index(&frame.argument(4), b_end);
    let result = a.with_bytes(|a| {
        b.with_bytes(|b| encoding::compare(part(a, a_start, a_end), part(b, b_start, b_end)))
    });
    Ok(JSValue::js_number_from_int32(result))
}

/// `copy(source, sourceStart, target, targetStart, count)`: copies what fits, as `memmove`; how many bytes.
fn copy(global: &JSGlobalObject, frame: &CallFrame<'_>) -> JsResult<JSValue> {
    let source = view(global, &frame.argument(0))?;
    let target = view(global, &frame.argument(2))?;
    let source_start = index(&frame.argument(1), source.byte_len());
    let target_start = index(&frame.argument(3), target.byte_len());
    let count = index(&frame.argument(4), usize::MAX)
        .min(source.byte_len() - source_start)
        .min(target.byte_len() - target_start);
    if count == 0 {
        return Ok(JSValue::js_number(0.0));
    }
    let bytes = source.with_bytes(|bytes| part(bytes, source_start, source_start + count).to_vec());
    target.with_bytes_mut(|to| {
        let to = part_mut(to, target_start, target_start + bytes.len());
        to.copy_from_slice(&bytes[..to.len()]);
    });
    Ok(JSValue::js_number(bytes.len() as f64))
}

/// `swap(view, size)`: reverses each `size` bytes in place.
fn swap(global: &JSGlobalObject, frame: &CallFrame<'_>) -> JsResult<JSValue> {
    let target = view(global, &frame.argument(0))?;
    let size = index(&frame.argument(1), 8).max(1);
    target.with_bytes_mut(|bytes| encoding::swap(bytes, size));
    Ok(frame.argument(0))
}

fn is_utf8(global: &JSGlobalObject, frame: &CallFrame<'_>) -> JsResult<JSValue> {
    let source = view(global, &frame.argument(0))?;
    Ok(JSValue::js_boolean(source.with_bytes(encoding::is_utf8)))
}

fn is_ascii(global: &JSGlobalObject, frame: &CallFrame<'_>) -> JsResult<JSValue> {
    let source = view(global, &frame.argument(0))?;
    Ok(JSValue::js_boolean(source.with_bytes(encoding::is_ascii)))
}

/// `transcode(view, from, to)`: a new `Uint8Array`, or the name of the ICU status it failed with.
fn transcode(global: &JSGlobalObject, frame: &CallFrame<'_>) -> JsResult<JSValue> {
    let source = view(global, &frame.argument(0))?;
    let from = TranscodeEncoding::parse(&frame.argument(1).to_utf8(global)?);
    let to = TranscodeEncoding::parse(&frame.argument(2).to_utf8(global)?);
    match source.with_bytes(|bytes| encoding::transcode(bytes, from, to)) {
        Ok(bytes) => JSValue::create_uint8_array(global, &bytes),
        Err(error) => Ok(JSValue::from_utf8(error.name().as_bytes())),
    }
}

const FUNCTIONS: &[(&str, JSHostFn, usize)] = &[
    ("parseEncoding", parse_encoding, 1),
    ("byteLength", byte_length, 2),
    ("encode", encode, 2),
    ("write", write, 5),
    ("decode", decode, 4),
    ("fill", fill, 5),
    ("indexOf", index_of, 6),
    ("compare", compare, 6),
    ("copy", copy, 5),
    ("swap", swap, 2),
    ("isUtf8", is_utf8, 1),
    ("isAscii", is_ascii, 1),
    ("transcode", transcode, 3),
];

/// The object of these functions that buffer.ts builds `Buffer` on.
pub fn natives(global: &JSGlobalObject) -> JSValue {
    let object = JSObject::create(global);
    for &(name, f, length) in FUNCTIONS {
        object.put(global, name, JSFunction::create(global, name, f, length));
    }
    object.to_js()
}
