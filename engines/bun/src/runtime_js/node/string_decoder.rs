//! `node:string_decoder`'s `StringDecoder`: src/jsc/bindings/JSStringDecoder.cpp on `bun_js`.
//!
//! JavaScriptCore's constructor can also be called without `new` on an object, which it makes a decoder of (the
//! `StringDecoder.call(this)` of express's body-parser): a Boa constructor cannot see that `this`, so the module
//! (packages/bun-boa/src/js/intrinsics.ts) wraps this class in a function that does it, with [`DECODE`].

use boa_gc::{Finalize, Trace};
use bun_js::bindgen::determine_specific_type;
use bun_js::class::{Attributes, ClassBuilder, ClassRef};
use bun_js::{CallFrame, ErrorCode, JSGlobalObject, JSValue, JsClass, JsResult};

/// The property of an object that `StringDecoder.call(object)` made a decoder of, which holds the decoder: C++'s
/// private name `decode`.
pub const DECODE: &str = "__intrinsic__decode";

/// What `Bun__encoding__toString` decodes a decoder's bytes as: `BufferEncodingType` without `buffer`, and with
/// `ucs2` as `utf16le`, which every alias of it parses to.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Encoding {
    Utf8,
    Utf16le,
    Latin1,
    Ascii,
    Base64,
    Base64url,
    Hex,
}

impl Encoding {
    /// `parseEnumerationFromView<BufferEncodingType>`: the names Node knows, in any case.
    pub fn parse(name: &str) -> Option<Encoding> {
        const NAMES: &[(&str, Encoding)] = &[
            ("utf8", Encoding::Utf8),
            ("utf-8", Encoding::Utf8),
            ("ucs2", Encoding::Utf16le),
            ("ucs-2", Encoding::Utf16le),
            ("utf16le", Encoding::Utf16le),
            ("utf-16le", Encoding::Utf16le),
            ("latin1", Encoding::Latin1),
            ("binary", Encoding::Latin1),
            ("base64", Encoding::Base64),
            ("base64url", Encoding::Base64url),
            ("ascii", Encoding::Ascii),
            ("hex", Encoding::Hex),
            ("hex\0", Encoding::Hex),
        ];
        NAMES
            .iter()
            .find(|(spelling, _)| spelling.eq_ignore_ascii_case(name))
            .map(|&(_, encoding)| encoding)
    }

    /// `convertEnumerationToJS<BufferEncodingType>`.
    pub fn name(self) -> &'static str {
        match self {
            Encoding::Utf8 => "utf8",
            Encoding::Utf16le => "utf16le",
            Encoding::Latin1 => "latin1",
            Encoding::Ascii => "ascii",
            Encoding::Base64 => "base64",
            Encoding::Base64url => "base64url",
            Encoding::Hex => "hex",
        }
    }

    /// `Bun__encoding__toString`: `bytes` as a string, in UTF-16.
    pub fn decode(self, bytes: &[u8]) -> Vec<u16> {
        match self {
            Encoding::Utf8 => String::from_utf8_lossy(bytes).encode_utf16().collect(),
            Encoding::Utf16le => bytes
                .chunks_exact(2)
                .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
                .collect(),
            Encoding::Latin1 => bytes.iter().map(|&byte| u16::from(byte)).collect(),
            Encoding::Ascii => bytes.iter().map(|&byte| u16::from(byte & 0x7f)).collect(),
            Encoding::Hex => {
                const DIGITS: &[u8; 16] = b"0123456789abcdef";
                bytes
                    .iter()
                    .flat_map(|&byte| [DIGITS[usize::from(byte >> 4)], DIGITS[usize::from(byte & 0xf)]])
                    .map(u16::from)
                    .collect()
            }
            Encoding::Base64 | Encoding::Base64url => {
                let mut encoded = vec![0; bun_core::base64::encode_len(bytes)];
                let written = bun_core::base64::encode(&mut encoded, bytes);
                encoded.truncate(written);
                if self == Encoding::Base64url {
                    // The URL alphabet, without padding.
                    while encoded.last() == Some(&b'=') {
                        encoded.pop();
                    }
                    for byte in &mut encoded {
                        *byte = match *byte {
                            b'+' => b'-',
                            b'/' => b'_',
                            other => other,
                        };
                    }
                }
                encoded.into_iter().map(u16::from).collect()
            }
        }
    }
}

/// Whether a UTF-8 byte is ASCII (0), starts a sequence of 2, 3 or 4, continues one (-1), or cannot be (-2).
fn utf8_check_byte(byte: u8) -> i8 {
    if byte <= 0x7f {
        0
    } else if byte >> 5 == 0x06 {
        2
    } else if byte >> 4 == 0x0e {
        3
    } else if byte >> 3 == 0x1e {
        4
    } else if byte >> 6 == 0x02 {
        -1
    } else {
        -2
    }
}

fn is_continuation(byte: u8) -> bool {
    byte & 0xc0 == 0x80
}

/// The state of a decoder: the bytes of a character that a chunk ended in the middle of.
#[derive(Trace, Finalize)]
pub struct StringDecoder {
    #[unsafe_ignore_trace]
    encoding: Encoding,
    #[unsafe_ignore_trace]
    last_char: [u8; 4],
    #[unsafe_ignore_trace]
    last_need: u32,
    #[unsafe_ignore_trace]
    last_total: u32,
}

impl StringDecoder {
    pub fn new(encoding: Encoding) -> StringDecoder {
        StringDecoder {
            encoding,
            last_char: [0; 4],
            last_need: 0,
            last_total: 0,
        }
    }

    pub fn encoding(&self) -> Encoding {
        self.encoding
    }

    fn decode(&self, bytes: &[u8]) -> Vec<u16> {
        self.encoding.decode(bytes)
    }

    /// The bytes (2, 3 or 4) the UTF-8 character that `buf[i..]` ends in the middle of needs in all, from at most
    /// its last 3 bytes; 0 if it ends no such character.
    fn utf8_check_incomplete(&mut self, buf: &[u8], i: usize) -> u32 {
        let Some(mut j) = buf.len().checked_sub(1) else {
            return 0;
        };
        if j < i {
            return 0;
        }
        let mut nb = utf8_check_byte(buf[j]);
        if nb >= 0 {
            if nb > 0 {
                self.last_need = nb as u32 - 1;
            }
            return nb as u32;
        }
        if j == 0 || nb == -2 {
            return 0;
        }
        j -= 1;
        if j < i {
            return 0;
        }
        nb = utf8_check_byte(buf[j]);
        if nb >= 0 {
            if nb > 0 {
                self.last_need = (nb as u32).wrapping_sub(2);
            }
            return nb as u32;
        }
        if j == 0 || nb == -2 {
            return 0;
        }
        j -= 1;
        if j < i {
            return 0;
        }
        nb = utf8_check_byte(buf[j]);
        if nb >= 0 {
            if nb > 0 {
                if nb == 2 {
                    nb = 0;
                } else {
                    self.last_need = nb as u32 - 3;
                }
            }
            return nb as u32;
        }
        0
    }

    fn fill_last(&mut self, buf: &[u8]) -> Vec<u16> {
        let length = buf.len() as u32;
        let at = (self.last_total - self.last_need) as usize;
        if self.encoding == Encoding::Utf8 {
            // A byte that starts a character where the last one needs a continuation: the last one is an error by
            // itself, and the rest of `buf` is decoded from there.
            for i in 0..length.min(self.last_need) {
                if !is_continuation(buf[i as usize]) {
                    let chars = at + i as usize;
                    self.last_char[at..chars].copy_from_slice(&buf[..i as usize]);
                    self.last_need = i;
                    return self.decode(&self.last_char[..chars]);
                }
            }
        }
        if self.last_need <= length {
            let need = self.last_need as usize;
            self.last_char[at..at + need].copy_from_slice(&buf[..need]);
            return self.decode(&self.last_char[..self.last_total as usize]);
        }
        self.last_char[at..at + buf.len()].copy_from_slice(buf);
        if self.encoding == Encoding::Utf8 {
            let last_last_need = self.last_need;
            let filled = (self.last_total - last_last_need + length) as usize;
            let last_char = self.last_char;
            let total = self.utf8_check_incomplete(&last_char[..filled], 0);
            if total == 0 {
                let len = (self.last_total - self.last_need + length) as usize;
                self.last_need = length;
                return self.decode(&self.last_char[..len]);
            }
            self.last_need = last_last_need;
        }
        self.last_need -= length;
        Vec::new()
    }

    fn text(&mut self, buf: &[u8], offset: usize) -> Vec<u16> {
        let length = buf.len();
        match self.encoding {
            Encoding::Utf16le => {
                if length == offset {
                    return Vec::new();
                }
                if (length - offset) % 2 == 0 {
                    let unit = u16::from_le_bytes([buf[length - 2], buf[length - 1]]);
                    if (0xd800..=0xdbff).contains(&unit) {
                        self.last_need = 2;
                        self.last_total = 4;
                        self.last_char[0] = buf[length - 2];
                        self.last_char[1] = buf[length - 1];
                        return self.decode(&buf[offset..length - 2]);
                    }
                    return self.decode(&buf[offset..]);
                }
                self.last_need = 1;
                self.last_total = 2;
                self.last_char[0] = buf[length - 1];
                self.decode(&buf[offset..length - 1])
            }
            Encoding::Utf8 => {
                let total = self.utf8_check_incomplete(buf, offset);
                if self.last_need == 0 {
                    return self.decode(&buf[offset..]);
                }
                self.last_total = total;
                let end = length - (total - self.last_need) as usize;
                if end < length {
                    let n = (length - end).min(4);
                    self.last_char[..n].copy_from_slice(&buf[end..end + n]);
                }
                self.decode(&buf[offset..end])
            }
            Encoding::Base64 | Encoding::Base64url => {
                let n = (length - offset) % 3;
                if n == 0 {
                    return self.decode(&buf[offset..]);
                }
                self.last_need = 3 - n as u32;
                self.last_total = 3;
                if n == 1 {
                    self.last_char[0] = buf[length - 1];
                } else {
                    self.last_char[0] = buf[length - 2];
                    self.last_char[1] = buf[length - 1];
                }
                self.decode(&buf[offset..length - n])
            }
            Encoding::Latin1 | Encoding::Ascii | Encoding::Hex => unreachable!("decoded whole by write"),
        }
    }

    /// `decoder.write(buf)`.
    pub fn write(&mut self, buf: &[u8]) -> Vec<u16> {
        if buf.is_empty() {
            return Vec::new();
        }
        match self.encoding {
            Encoding::Utf16le | Encoding::Utf8 | Encoding::Base64 | Encoding::Base64url => {
                if self.last_need == 0 {
                    return self.text(buf, 0);
                }
                let mut first_half = self.fill_last(buf);
                if first_half.is_empty() {
                    return first_half;
                }
                let offset = self.last_need as usize;
                self.last_need = 0;
                self.last_total = 0;
                if offset == buf.len() {
                    return first_half;
                }
                first_half.extend(self.text(buf, offset));
                first_half
            }
            Encoding::Latin1 | Encoding::Ascii | Encoding::Hex => self.decode(buf),
        }
    }

    /// `decoder.end(buf)`: what is left, as the encoding's replacement where a character is incomplete.
    pub fn end(&mut self, buf: &[u8]) -> Vec<u16> {
        let result = self.end_unreset(buf);
        // As Node's FlushData: the bytes of `lastChar` stay.
        self.last_total = 0;
        self.last_need = 0;
        result
    }

    fn end_unreset(&mut self, buf: &[u8]) -> Vec<u16> {
        let rest = |this: &Self| -> usize {
            match this.encoding {
                Encoding::Base64 | Encoding::Base64url => 3 - this.last_need as usize,
                _ => (this.last_total - this.last_need) as usize,
            }
        };
        match self.encoding {
            Encoding::Utf16le | Encoding::Utf8 | Encoding::Base64 | Encoding::Base64url => {
                if buf.is_empty() {
                    if self.last_need != 0 {
                        return self.decode(&self.last_char[..rest(self)]);
                    }
                    return Vec::new();
                }
                let mut first_half = self.write(buf);
                if self.last_need != 0 {
                    first_half.extend(self.decode(&self.last_char[..rest(self)]));
                }
                first_half
            }
            Encoding::Latin1 | Encoding::Ascii | Encoding::Hex => self.write(buf),
        }
    }
}

// ───────────────────────────── JavaScript ─────────────────────────────

/// C++'s `jsStringDecoderCast`: `this`, or the decoder an object that `StringDecoder.call` made one of holds.
fn decoder_of(global: &JSGlobalObject, this: &JSValue, function: &str) -> JsResult<ClassRef<StringDecoder>> {
    if let Some(decoder) = this.as_class_ref::<StringDecoder>() {
        return Ok(decoder);
    }
    if let Some(decoder) = this.get(global, DECODE)?.and_then(|held| held.as_class_ref::<StringDecoder>()) {
        return Ok(decoder);
    }
    Err(ErrorCode::INVALID_THIS.throw(
        global,
        format_args!("Can only call StringDecoder.{function} on instances of StringDecoder"),
    ))
}

/// The accessors are DOM attributes in C++: only an instance has them.
fn instance_of(global: &JSGlobalObject, this: &JSValue, accessor: &str) -> JsResult<ClassRef<StringDecoder>> {
    this.as_class_ref::<StringDecoder>().ok_or_else(|| {
        global.throw_type_error(format_args!(
            "The StringDecoder.{accessor} getter can only be used on instances of StringDecoder"
        ))
    })
}

fn not_enough_arguments(global: &JSGlobalObject) -> bun_js::JsError {
    ErrorCode::MISSING_ARGS.throw(global, format_args!("Not enough arguments"))
}

fn expected_uint8_array(global: &JSGlobalObject) -> bun_js::JsError {
    global.throw_type_error(format_args!("Expected Uint8Array"))
}

fn string(units: Vec<u16>) -> JSValue {
    JSValue::from_utf16(&units)
}

fn write(global: &JSGlobalObject, frame: &CallFrame<'_>) -> JsResult<JSValue> {
    let decoder = decoder_of(global, frame.this(), "write")?;
    if frame.arguments_count() < 1 {
        return Err(not_enough_arguments(global));
    }
    let buffer = frame.argument(0);
    let Some(bytes) = buffer.array_buffer_view_bytes(global)? else {
        // As Node: `if (typeof buf === 'string') return buf`.
        if buffer.is_string() {
            return Ok(buffer);
        }
        let received = determine_specific_type(global, &buffer)?;
        return Err(ErrorCode::INVALID_ARG_TYPE.throw(
            global,
            format_args!(
                "The \"buf\" argument must be of type Buffer, TypedArray, or DataView. Received {received}"
            ),
        ));
    };
    Ok(string(decoder.borrow_mut(global)?.write(&bytes)))
}

fn end(global: &JSGlobalObject, frame: &CallFrame<'_>) -> JsResult<JSValue> {
    let decoder = decoder_of(global, frame.this(), "end")?;
    if frame.arguments_count() < 1 {
        return Ok(string(decoder.borrow_mut(global)?.end(&[])));
    }
    let Some(bytes) = frame.argument(0).array_buffer_view_bytes(global)? else {
        return Err(expected_uint8_array(global));
    };
    Ok(string(decoder.borrow_mut(global)?.end(&bytes)))
}

fn text(global: &JSGlobalObject, frame: &CallFrame<'_>) -> JsResult<JSValue> {
    let decoder = decoder_of(global, frame.this(), "text")?;
    if frame.arguments_count() < 2 {
        return Err(not_enough_arguments(global));
    }
    let Some(bytes) = frame.argument(0).array_buffer_view_bytes(global)? else {
        return Err(expected_uint8_array(global));
    };
    let offset = frame.argument(1).coerce_to_i32(global)?;
    let Some(rest) = usize::try_from(offset).ok().and_then(|offset| bytes.get(offset..)) else {
        return Ok(JSValue::js_empty_string(global));
    };
    Ok(string(decoder.borrow_mut(global)?.write(rest)))
}

fn last_char(global: &JSGlobalObject, frame: &CallFrame<'_>) -> JsResult<JSValue> {
    let decoder = instance_of(global, frame.this(), "lastChar")?;
    let bytes = decoder.borrow(global)?.last_char;
    JSValue::create_buffer(global, &bytes)
}

fn last_need(global: &JSGlobalObject, frame: &CallFrame<'_>) -> JsResult<JSValue> {
    let decoder = instance_of(global, frame.this(), "lastNeed")?;
    Ok(JSValue::js_number(f64::from(decoder.borrow(global)?.last_need)))
}

fn last_total(global: &JSGlobalObject, frame: &CallFrame<'_>) -> JsResult<JSValue> {
    let decoder = instance_of(global, frame.this(), "lastTotal")?;
    Ok(JSValue::js_number(f64::from(decoder.borrow(global)?.last_total)))
}

fn encoding(global: &JSGlobalObject, frame: &CallFrame<'_>) -> JsResult<JSValue> {
    let decoder = instance_of(global, frame.this(), "encoding")?;
    Ok(JSValue::from_utf8(decoder.borrow(global)?.encoding.name().as_bytes()))
}

impl JsClass for StringDecoder {
    const NAME: &'static str = "StringDecoder";

    /// C++'s `createDecoder`: UTF-8 unless an encoding is named; one it does not know throws
    /// `ERR_UNKNOWN_ENCODING`.
    fn construct(global: &JSGlobalObject, frame: &CallFrame<'_>) -> JsResult<Self> {
        let argument = frame.argument(0);
        if argument.is_undefined_or_null() {
            return Ok(StringDecoder::new(Encoding::Utf8));
        }
        let parsed = argument.as_string().and_then(|name| Encoding::parse(&name));
        match parsed {
            Some(encoding) => Ok(StringDecoder::new(encoding)),
            None => {
                let name = argument.to_utf8(global)?;
                Err(ErrorCode::UNKNOWN_ENCODING.throw(global, format_args!("Unknown encoding: {name}")))
            }
        }
    }

    fn define(class: &mut ClassBuilder<'_, Self>) -> JsResult<()> {
        let accessor = Attributes {
            writable: false,
            enumerable: true,
            configurable: false,
        };
        class.prototype_getter("lastChar", accessor, last_char)?;
        class.prototype_getter("lastNeed", accessor, last_need)?;
        class.prototype_getter("lastTotal", accessor, last_total)?;
        class.prototype_getter("encoding", accessor, encoding)?;
        let function = Attributes {
            writable: true,
            enumerable: true,
            configurable: true,
        };
        class.prototype_function("write", 1, function, write)?;
        class.prototype_function("end", 1, function, end)?;
        class.prototype_function("text", 2, function, text)
    }
}

/// The class's constructor, which the module wraps.
pub fn constructor(global: &JSGlobalObject) -> JsResult<JSValue> {
    bun_js::class::constructor::<StringDecoder>(global)
}
