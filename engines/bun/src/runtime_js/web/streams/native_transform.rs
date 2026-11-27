//! The transform streams whose transformer is Bun's: `TextEncoderStream`, `TextDecoderStream` (JSTextEncoderStream.cpp,
//! JSTextDecoderStream.cpp, Encoding §5) and `CompressionStream`, `DecompressionStream` (JSCompressionStream.cpp,
//! JSDecompressionStream.cpp, Compression §5) over bun_deflate and bun_zstd.
//!
//! bun_deflate and bun_zstd compress and decompress a whole input: (de)compression buffers what is written and
//! enqueues the result when the writable side closes.

use boa_gc::{Finalize, Trace};
use bun_js::class::{Attributes, ClassBuilder};
use bun_js::{CallFrame, ErrorCode, JSGlobalObject, JSValue, JsClass, JsResult};

use super::internals::member;
use super::transform_stream;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Codec {
    Deflate,
    DeflateRaw,
    Gzip,
    Zstd,
}

/// A transformer's state between chunks.
#[derive(Clone, Trace, Finalize)]
#[boa_gc(unsafe_no_drop)]
pub enum NativeTransform {
    /// A high surrogate that ended the last chunk, which the next one may pair.
    TextEncoder { pending: Option<u16> },
    /// The bytes of a sequence that the last chunk cut.
    TextDecoder { pending: Vec<u8>, fatal: bool, ignore_bom: bool, seen_text: bool },
    Compress {
        #[unsafe_ignore_trace]
        codec: Codec,
        input: Vec<u8>,
    },
    Decompress {
        #[unsafe_ignore_trace]
        codec: Codec,
        input: Vec<u8>,
    },
}

fn buffer_source(global: &JSGlobalObject, chunk: &JSValue) -> JsResult<Vec<u8>> {
    if chunk.is_undefined_or_null() {
        return Err(ErrorCode::STREAM_NULL_VALUES.throw(global, format_args!("May not write null values to stream")));
    }
    match crate::engine::buffer_source_bytes(global, chunk) {
        Some(bytes) => Ok(bytes),
        None => {
            let received = bun_js::bindgen::determine_specific_type(global, chunk)?;
            Err(ErrorCode::INVALID_ARG_TYPE.throw(
                global,
                format_args!("The \"chunk\" argument must be of type BufferSource. Received {received}"),
            ))
        }
    }
}

fn invalid_data(global: &JSGlobalObject) -> bun_js::JsError {
    ErrorCode::ENCODING_INVALID_ENCODED_DATA
        .throw(global, format_args!("The encoded data was not valid for encoding utf-8"))
}

/// Decodes what is complete of `pending`; an incomplete sequence at the end stays for the next chunk.
fn decode_utf8(global: &JSGlobalObject, pending: &mut Vec<u8>, fatal: bool, end: bool) -> JsResult<String> {
    let mut text = String::new();
    let mut rest: &[u8] = pending;
    loop {
        match std::str::from_utf8(rest) {
            Ok(valid) => {
                text.push_str(valid);
                rest = &[];
                break;
            }
            Err(error) => {
                let (valid, after) = rest.split_at(error.valid_up_to());
                text.push_str(std::str::from_utf8(valid).unwrap_or_default());
                match error.error_len() {
                    Some(len) => {
                        if fatal {
                            return Err(invalid_data(global));
                        }
                        text.push('\u{FFFD}');
                        rest = &after[len..];
                    }
                    None if end => {
                        if fatal {
                            return Err(invalid_data(global));
                        }
                        text.push('\u{FFFD}');
                        rest = &[];
                        break;
                    }
                    None => {
                        rest = after;
                        break;
                    }
                }
            }
        }
    }
    *pending = rest.to_vec();
    Ok(text)
}

fn encode_utf16(units: &[u16]) -> Vec<u8> {
    String::from_utf16_lossy(units).into_bytes()
}

fn is_high_surrogate(unit: u16) -> bool {
    (0xD800..0xDC00).contains(&unit)
}

fn compress(global: &JSGlobalObject, codec: Codec, input: &[u8]) -> JsResult<Vec<u8>> {
    let result = match codec {
        Codec::Deflate => bun_deflate::compress_to_vec(input, bun_deflate::Format::Zlib, bun_deflate::Level::DEFAULT).ok(),
        Codec::DeflateRaw => bun_deflate::compress_to_vec(input, bun_deflate::Format::Raw, bun_deflate::Level::DEFAULT).ok(),
        Codec::Gzip => bun_deflate::compress_to_vec(input, bun_deflate::Format::Gzip, bun_deflate::Level::DEFAULT).ok(),
        Codec::Zstd => {
            let mut out = Vec::new();
            bun_zstd::compress_append(&mut out, input, bun_zstd::DEFAULT_LEVEL).ok().map(|()| out)
        }
    };
    result.ok_or_else(|| global.throw_type_error(format_args!("compression failed")))
}

const INFLATE_LIMIT: usize = 1 << 31;

fn adler32(bytes: &[u8]) -> u32 {
    let (mut a, mut b) = (1u32, 0u32);
    for chunk in bytes.chunks(5552) {
        for &byte in chunk {
            a += u32::from(byte);
            b += a;
        }
        a %= 65521;
        b %= 65521;
    }
    (b << 16) | a
}

/// zlib (RFC 1950): a two-byte header, a raw DEFLATE stream, and the Adler-32 of what it holds.
fn inflate_zlib(input: &[u8]) -> Option<Vec<u8>> {
    let [cmf, flg, ..] = *input else { return None };
    if cmf & 0x0F != 8 || (u16::from(cmf) * 256 + u16::from(flg)) % 31 != 0 || flg & 0x20 != 0 {
        return None;
    }
    let mut out = Vec::new();
    let raw = input.get(2..)?;
    let decompressed = bun_deflate::inflate(
        raw,
        bun_deflate::Output::Vec {
            vec: &mut out,
            limit: INFLATE_LIMIT,
        },
    )
    .ok()?;
    let trailer = raw.get(decompressed.read..decompressed.read + 4)?;
    (u32::from_be_bytes(trailer.try_into().ok()?) == adler32(&out)).then_some(out)
}

fn decompress(global: &JSGlobalObject, codec: Codec, input: &[u8]) -> JsResult<Vec<u8>> {
    let result = match codec {
        Codec::Deflate => inflate_zlib(input),
        Codec::DeflateRaw => {
            let mut out = Vec::new();
            bun_deflate::inflate(input, bun_deflate::Output::Vec { vec: &mut out, limit: INFLATE_LIMIT })
                .ok()
                .map(|_| out)
        }
        Codec::Gzip => {
            let mut out = Vec::new();
            bun_deflate::gunzip(input, bun_deflate::Output::Vec { vec: &mut out, limit: INFLATE_LIMIT })
                .ok()
                .map(|_| out)
        }
        Codec::Zstd => bun_zstd::decompress_alloc(input).ok(),
    };
    result.ok_or_else(|| {
        let what = if codec == Codec::Zstd { "zstd decompression failed" } else { "inflate failed" };
        global.throw_type_error(format_args!("{what}"))
    })
}

/// What a chunk makes: the chunks to enqueue.
pub fn transform(global: &JSGlobalObject, state: &mut NativeTransform, chunk: &JSValue) -> JsResult<Vec<Vec<u8>>> {
    match state {
        NativeTransform::TextEncoder { pending } => {
            let mut units: Vec<u16> = pending.take().into_iter().collect();
            units.extend(chunk.to_utf16(global)?);
            if units.last().is_some_and(|&unit| is_high_surrogate(unit)) {
                *pending = units.pop();
            }
            let bytes = encode_utf16(&units);
            Ok(if bytes.is_empty() { vec![] } else { vec![bytes] })
        }
        NativeTransform::TextDecoder { pending, fatal, ignore_bom, seen_text } => {
            pending.extend(buffer_source(global, chunk)?);
            let text = decode_utf8(global, pending, *fatal, false)?;
            Ok(text_chunk(text, *ignore_bom, seen_text))
        }
        NativeTransform::Compress { input, .. } | NativeTransform::Decompress { input, .. } => {
            input.extend(buffer_source(global, chunk)?);
            Ok(vec![])
        }
    }
}

/// A decoded chunk, as UTF-8, without the BOM that starts the stream unless `ignoreBOM`.
fn text_chunk(mut text: String, ignore_bom: bool, seen_text: &mut bool) -> Vec<Vec<u8>> {
    if !*seen_text && !text.is_empty() {
        *seen_text = true;
        if !ignore_bom && text.starts_with('\u{FEFF}') {
            text.drain(..'\u{FEFF}'.len_utf8());
        }
    }
    if text.is_empty() { vec![] } else { vec![text.into_bytes()] }
}

/// What the end of the writable side makes.
pub fn flush(global: &JSGlobalObject, state: &mut NativeTransform) -> JsResult<Vec<Vec<u8>>> {
    match state {
        NativeTransform::TextEncoder { pending } => Ok(match pending.take() {
            Some(_) => vec!["\u{FFFD}".as_bytes().to_vec()],
            None => vec![],
        }),
        NativeTransform::TextDecoder { pending, fatal, ignore_bom, seen_text } => {
            let text = decode_utf8(global, pending, *fatal, true)?;
            Ok(text_chunk(text, *ignore_bom, seen_text))
        }
        NativeTransform::Compress { codec, input } => {
            let out = compress(global, *codec, &std::mem::take(input))?;
            Ok(if out.is_empty() { vec![] } else { vec![out] })
        }
        NativeTransform::Decompress { codec, input } => {
            let out = decompress(global, *codec, &std::mem::take(input))?;
            Ok(if out.is_empty() { vec![] } else { vec![out] })
        }
    }
}

/// Whether the chunks of `state` are strings (a TextDecoderStream's) rather than `Uint8Array`s.
pub fn yields_text(state: &NativeTransform) -> bool {
    matches!(state, NativeTransform::TextDecoder { .. })
}

fn codec_of(global: &JSGlobalObject, format: &JSValue) -> JsResult<Codec> {
    let name = match format.is_undefined() {
        true => None,
        false => Some(format.to_utf8(global)?),
    };
    match name.as_deref() {
        Some("deflate") => Ok(Codec::Deflate),
        Some("deflate-raw") => Ok(Codec::DeflateRaw),
        Some("gzip") => Ok(Codec::Gzip),
        Some("zstd") => Ok(Codec::Zstd),
        Some("brotli") => Err(global.throw_type_error(format_args!("brotli is not implemented on Boa yet"))),
        Some(other) => Err(ErrorCode::INVALID_ARG_VALUE.throw(
            global,
            format_args!(
                "The argument 'format' must be one of: deflate, deflate-raw, gzip, brotli, zstd. Received '{other}'"
            ),
        )),
        None => Err(ErrorCode::INVALID_ARG_VALUE.throw(
            global,
            format_args!("The argument 'format' must be one of: deflate, deflate-raw, gzip, brotli, zstd. Received undefined"),
        )),
    }
}

/// The readable and writable sides of a stream whose transform is Rust.
#[derive(Trace, Finalize)]
pub struct Sides {
    readable: JSValue,
    writable: JSValue,
    #[unsafe_ignore_trace]
    fatal: bool,
    #[unsafe_ignore_trace]
    ignore_bom: bool,
}

impl Sides {
    fn new() -> Sides {
        Sides {
            readable: JSValue::UNDEFINED,
            writable: JSValue::UNDEFINED,
            fatal: false,
            ignore_bom: false,
        }
    }
}

fn set_sides(global: &JSGlobalObject, sides: &mut Sides, transform: &JSValue) -> JsResult<()> {
    let t = transform_stream::stream_of(transform);
    let t = t.borrow(global)?;
    sides.readable = t.readable.clone();
    sides.writable = t.writable.clone();
    Ok(())
}

macro_rules! sides_class {
    ($name:ident, $js_name:literal, $length:literal, $state:expr, $extra:expr) => {
        #[derive(Trace, Finalize)]
        pub struct $name(Sides);

        impl JsClass for $name {
            const NAME: &'static str = $js_name;
            const LENGTH: usize = $length;

            fn construct(global: &JSGlobalObject, frame: &CallFrame<'_>) -> JsResult<Self> {
                let state: fn(&JSGlobalObject, &CallFrame<'_>, &mut Sides) -> JsResult<NativeTransform> = $state;
                let mut sides = Sides::new();
                let state = state(global, frame, &mut sides)?;
                let transform = transform_stream::create_native(global, state)?;
                set_sides(global, &mut sides, &transform)?;
                Ok($name(sides))
            }

            fn define(class: &mut ClassBuilder<'_, Self>) -> JsResult<()> {
                let attributes = Attributes::DEFAULT;
                class.getter("readable", attributes, |this, global| Ok(this.borrow(global)?.0.readable.clone()))?;
                class.getter("writable", attributes, |this, global| Ok(this.borrow(global)?.0.writable.clone()))?;
                let extra: fn(&mut ClassBuilder<'_, $name>) -> JsResult<()> = $extra;
                extra(class)
            }
        }
    };
}

fn utf8_encoding(class: &mut ClassBuilder<'_, TextEncoderStream>) -> JsResult<()> {
    class.getter("encoding", Attributes::DEFAULT, |_, _| Ok(JSValue::from_utf8(b"utf-8")))
}

sides_class!(TextEncoderStream, "TextEncoderStream", 0, |_, _, _| Ok(NativeTransform::TextEncoder { pending: None }), utf8_encoding);

fn decoder_state(global: &JSGlobalObject, frame: &CallFrame<'_>, sides: &mut Sides) -> JsResult<NativeTransform> {
    let [label, options] = frame.arguments_as_array::<2>();
    if !label.is_undefined() {
        let label = label.to_utf8(global)?;
        let normalized = label.trim().to_ascii_lowercase();
        if !matches!(normalized.as_str(), "utf-8" | "utf8" | "unicode-1-1-utf-8" | "unicode11utf8" | "unicode20utf8" | "x-unicode20utf8") {
            return Err(ErrorCode::ENCODING_NOT_SUPPORTED
                .throw(global, format_args!("Unsupported encoding label \"{label}\"")));
        }
    }
    if !options.is_undefined_or_null() && !options.is_object() {
        return Err(global.throw_type_error(format_args!("The \"options\" argument must be of type object")));
    }
    sides.fatal = member(global, &options, "fatal")?.to_boolean();
    sides.ignore_bom = member(global, &options, "ignoreBOM")?.to_boolean();
    Ok(NativeTransform::TextDecoder {
        pending: Vec::new(),
        fatal: sides.fatal,
        ignore_bom: sides.ignore_bom,
        seen_text: false,
    })
}

fn decoder_members(class: &mut ClassBuilder<'_, TextDecoderStream>) -> JsResult<()> {
    let attributes = Attributes::DEFAULT;
    class.getter("encoding", attributes, |_, _| Ok(JSValue::from_utf8(b"utf-8")))?;
    class.getter("fatal", attributes, |this, global| Ok(JSValue::js_boolean(this.borrow(global)?.0.fatal)))?;
    class.getter("ignoreBOM", attributes, |this, global| Ok(JSValue::js_boolean(this.borrow(global)?.0.ignore_bom)))?;
    Ok(())
}

sides_class!(TextDecoderStream, "TextDecoderStream", 0, decoder_state, decoder_members);

sides_class!(
    CompressionStream,
    "CompressionStream",
    1,
    |global, frame, _| Ok(NativeTransform::Compress { codec: codec_of(global, &frame.argument(0))?, input: Vec::new() }),
    |_| Ok(())
);

sides_class!(
    DecompressionStream,
    "DecompressionStream",
    1,
    |global, frame, _| Ok(NativeTransform::Decompress { codec: codec_of(global, &frame.argument(0))?, input: Vec::new() }),
    |_| Ok(())
);
