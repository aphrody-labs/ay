//! `TextEncoder` (src/runtime/webcore/TextEncoder.rs, JSTextEncoder.cpp) on bun_js: UTF-8 with U+FFFD for each
//! lone surrogate.

use boa_gc::{Finalize, Trace};
use bun_core::strings;
use bun_js::class::{Attributes, ClassBuilder};
use bun_js::{
    CallFrame, ClassRef, JSGlobalObject, JSObject, JSValue, JsClass, JsResult, StringUnits, TypedArrayType,
};

#[derive(Trace, Finalize)]
pub struct TextEncoder;

/// `ToString(value)`, as its code units.
fn with_units<R>(global: &JSGlobalObject, value: &JSValue, f: impl FnOnce(StringUnits<'_>) -> R) -> JsResult<R> {
    if value.is_string() {
        return Ok(value.with_string_units(f).expect("a string has code units"));
    }
    let units = value.to_utf16(global)?;
    Ok(f(StringUnits::Utf16(&units)))
}

fn utf8(units: StringUnits<'_>) -> Vec<u8> {
    match units {
        StringUnits::Latin1(units) => strings::allocate_latin1_into_utf8(units).unwrap_or_default(),
        StringUnits::Utf16(units) => strings::to_utf8_alloc_with_type(units),
    }
}

impl JsClass for TextEncoder {
    const NAME: &'static str = "TextEncoder";

    fn construct(_: &JSGlobalObject, _: &CallFrame<'_>) -> JsResult<Self> {
        Ok(TextEncoder)
    }

    fn define(class: &mut ClassBuilder<'_, Self>) -> JsResult<()> {
        class.method("encode", 0, Attributes::DEFAULT, encode)?;
        class.method("encodeInto", 2, Attributes::DEFAULT, encode_into)?;
        class.getter("encoding", Attributes::DEFAULT, |_, _| Ok(JSValue::from_utf8(b"utf-8")))
    }
}

/// `encode(input = "")`.
fn encode(_: &ClassRef<TextEncoder>, global: &JSGlobalObject, frame: &CallFrame<'_>) -> JsResult<JSValue> {
    let input = frame.argument(0);
    if input.is_undefined() {
        return JSValue::create_uint8_array(global, &[]);
    }
    let bytes = with_units(global, &input, utf8)?;
    JSValue::create_uint8_array(global, &bytes)
}

/// `encodeInto(source, destination)`: the whole code points of `source` that fit in `destination`, and how many
/// code units and bytes that is.
fn encode_into(_: &ClassRef<TextEncoder>, global: &JSGlobalObject, frame: &CallFrame<'_>) -> JsResult<JSValue> {
    if frame.arguments_count() < 2 {
        return Err(global.throw_type_error(format_args!("Not enough arguments")));
    }
    let source = frame.argument(0);
    let destination = frame.argument(1);
    let source = if source.is_string() { source } else { JSValue::from_utf16(&source.to_utf16(global)?) };
    let target = match destination.as_array_buffer(global)? {
        Some(view) if view.typed_array_type() != TypedArrayType::ArrayBuffer => view,
        _ => return Err(global.throw_type_error(format_args!("Expected Uint8Array"))),
    };
    let result = with_units(global, &source, |units| {
        target.with_bytes_mut(|bytes| match units {
            StringUnits::Latin1(units) => strings::copy_latin1_into_utf8(bytes, units),
            StringUnits::Utf16(units) => strings::copy_utf16_into_utf8(bytes, units),
        })
    })?;
    let object = JSObject::create(global);
    object.put(global, "read", JSValue::js_number(result.read as f64));
    object.put(global, "written", JSValue::js_number(result.written as f64));
    Ok(object.to_js())
}

/// `globalThis.TextEncoder`, over Boa's WinterTC one.
pub(crate) fn install(context: &mut boa_engine::Context) -> boa_engine::JsResult<()> {
    let global = JSGlobalObject::from_context(context);
    let constructor = crate::lift(global, bun_js::class::constructor::<TextEncoder>(global))?;
    crate::define_global(context, "TextEncoder", constructor.into_engine())
}
