//! `TextDecoder` (src/runtime/webcore/TextDecoder.rs, TextDecoder.bind.ts) on bun_js. Every encoding goes through
//! encoding_rs, whose decoder keeps a stream's state (a partial sequence, a BOM not yet whole, ISO-2022-JP's mode)
//! across `{stream: true}` chunks, and removes the BOM of its own encoding unless `ignoreBOM`.

use boa_gc::{Finalize, Trace};
use bun_js::class::{Attributes, ClassBuilder};
use bun_js::{CallFrame, ClassRef, ErrorCode, JSGlobalObject, JSObject, JSValue, JsClass, JsResult};
use encoding_rs::{DecoderResult, Encoding};

#[derive(Trace, Finalize)]
pub struct TextDecoder {
    #[unsafe_ignore_trace]
    encoding: &'static Encoding,
    fatal: bool,
    ignore_bom: bool,
    /// https://encoding.spec.whatwg.org/#textdecoder-do-not-flush-flag: the last `decode()` was a `{stream: true}`
    /// chunk, so this one goes on with its decoder.
    do_not_flush: bool,
    #[unsafe_ignore_trace]
    decoder: Option<encoding_rs::Decoder>,
}

/// A WebIDL dictionary argument: `undefined` and `null` are none, anything else that is not an object is Node's
/// `ERR_INVALID_ARG_TYPE`.
fn dictionary(global: &JSGlobalObject, value: &JSValue, name: &str) -> JsResult<Option<JSObject>> {
    if value.is_undefined_or_null() {
        return Ok(None);
    }
    match value.get_object() {
        Some(object) => Ok(Some(object)),
        None => {
            let received = bun_js::bindgen::determine_specific_type(global, value)?;
            Err(ErrorCode::INVALID_ARG_TYPE.throw(
                global,
                format_args!("The \"{name}\" argument must be of type object. Received {received}"),
            ))
        }
    }
}

/// A `b.bool.loose.default(false)` member.
fn flag(global: &JSGlobalObject, options: &Option<JSObject>, key: &str) -> JsResult<bool> {
    match options {
        Some(options) => Ok(options.get(global, key)?.to_boolean()),
        None => Ok(false),
    }
}

/// The name `encoding` answers: the encoding's name in ASCII lowercase ("shift_jis").
fn label(encoding: &'static Encoding) -> String {
    encoding.name().to_ascii_lowercase()
}

impl TextDecoder {
    fn decode_chunk(&mut self, global: &JSGlobalObject, bytes: &[u8], stream: bool) -> JsResult<JSValue> {
        // https://encoding.spec.whatwg.org/#dom-textdecoder-decode steps 1-2: what follows a flush is a new stream.
        if !core::mem::replace(&mut self.do_not_flush, stream) {
            self.decoder = None;
        }
        // Nothing to decode. Also keeps the chunk away from encoding_rs 0.8.35, whose big5, euc-kr and shift_jis
        // decoders forget a pending lead byte when fed an empty non-final chunk.
        if bytes.is_empty() && stream {
            return Ok(JSValue::from_latin1(&[]));
        }
        let (encoding, ignore_bom, fatal) = (self.encoding, self.ignore_bom, self.fatal);
        let decoder = self.decoder.get_or_insert_with(|| {
            if ignore_bom {
                encoding.new_decoder_without_bom_handling()
            } else {
                encoding.new_decoder_with_bom_removal()
            }
        });
        let last = !stream;
        let Some(capacity) = decoder.max_utf16_buffer_length(bytes.len()) else {
            return Err(global.throw_out_of_memory());
        };
        let mut units = vec![0u16; capacity];
        let written = if fatal {
            let (result, _, written) = decoder.decode_to_utf16_without_replacement(bytes, &mut units, last);
            if let DecoderResult::Malformed(..) = result {
                // Drops what it had pending, so a later `{stream: true}` chunk does not go on from the bad byte.
                self.decoder = None;
                return Err(ErrorCode::ENCODING_INVALID_ENCODED_DATA.throw(
                    global,
                    format_args!("The encoded data was not valid for encoding {}", label(encoding)),
                ));
            }
            written
        } else {
            let (_, _, written, _) = decoder.decode_to_utf16(bytes, &mut units, last);
            written
        };
        units.truncate(written);
        Ok(JSValue::from_utf16(&units))
    }
}

impl JsClass for TextDecoder {
    const NAME: &'static str = "TextDecoder";

    /// `new TextDecoder(label = "utf-8", options = {})`: the label is a string first, then the options are checked,
    /// then the label is looked up.
    fn construct(global: &JSGlobalObject, frame: &CallFrame<'_>) -> JsResult<Self> {
        let name = frame.argument(0);
        let name = if name.is_undefined() { None } else { Some(name.to_utf8(global)?) };
        let options = dictionary(global, &frame.argument(1), "options")?;
        let fatal = flag(global, &options, "fatal")?;
        let ignore_bom = flag(global, &options, "ignoreBOM")?;
        let encoding = match &name {
            None => encoding_rs::UTF_8,
            // https://encoding.spec.whatwg.org/#dom-textdecoder: failure or replacement is a RangeError.
            Some(name) => match Encoding::for_label_no_replacement(name.as_bytes()) {
                Some(encoding) => encoding,
                None => {
                    return Err(ErrorCode::ENCODING_NOT_SUPPORTED
                        .throw(global, format_args!("Unsupported encoding label \"{name}\"")));
                }
            },
        };
        Ok(TextDecoder {
            encoding,
            fatal,
            ignore_bom,
            do_not_flush: false,
            decoder: None,
        })
    }

    fn define(class: &mut ClassBuilder<'_, Self>) -> JsResult<()> {
        class.method("decode", 0, Attributes::DEFAULT, decode)?;
        class.getter("encoding", Attributes::DEFAULT, |this, global| {
            Ok(JSValue::from_utf8(label(this.borrow(global)?.encoding).as_bytes()))
        })?;
        class.getter("fatal", Attributes::DEFAULT, |this, global| {
            Ok(JSValue::js_boolean(this.borrow(global)?.fatal))
        })?;
        class.getter("ignoreBOM", Attributes::DEFAULT, |this, global| {
            Ok(JSValue::js_boolean(this.borrow(global)?.ignore_bom))
        })
    }
}

/// `decode(input, options = {})`: the bytes are read once the options are, whose getters may write or detach them;
/// a SharedArrayBuffer's are a copy, as the bytes of any input are here.
fn decode(this: &ClassRef<TextDecoder>, global: &JSGlobalObject, frame: &CallFrame<'_>) -> JsResult<JSValue> {
    let input = frame.argument(0);
    let view = if input.is_undefined() {
        None
    } else {
        match input.as_array_buffer(global)? {
            Some(view) => Some(view),
            None => {
                return Err(ErrorCode::INVALID_ARG_TYPE.throw(
                    global,
                    format_args!("TextDecoder.decode expects an ArrayBuffer or TypedArray"),
                ));
            }
        }
    };
    let options = dictionary(global, &frame.argument(1), "options")?;
    let stream = flag(global, &options, "stream")?;
    let bytes = view.map_or_else(Vec::new, |view| view.with_bytes(<[u8]>::to_vec));
    this.borrow_mut(global)?.decode_chunk(global, &bytes, stream)
}

/// `globalThis.TextDecoder`, over Boa's WinterTC one.
pub(crate) fn install(context: &mut boa_engine::Context) -> boa_engine::JsResult<()> {
    let global = JSGlobalObject::from_context(context);
    let constructor = crate::lift(global, bun_js::class::constructor::<TextDecoder>(global))?;
    crate::define_global(context, "TextDecoder", constructor.into_engine())
}
