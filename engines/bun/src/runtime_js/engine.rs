//! What this crate needs of Boa that `bun_js` does not wrap yet: promises, the bytes of an `ArrayBuffer` or a view,
//! `JSON.parse`, `instanceof`. Each is one engine operation behind the `bun_jsc` shape that Blob.rs calls.

use boa_engine::object::builtins::{
    AlignedVec, JsArrayBuffer, JsDataView, JsPromise, JsSharedArrayBuffer, JsTypedArray,
    JsUint8Array,
};
use boa_engine::{Context, js_string};
use bun_js::{JSGlobalObject, JSValue, JsError, JsResult};

fn cx(global: &JSGlobalObject) -> &mut Context {
    // SAFETY: each caller uses the context for one Boa call and drops it before it calls anything else that takes
    // `global`.
    unsafe { global.context() }
}

/// `JSPromise::resolved_promise_value`.
pub(crate) fn resolved(global: &JSGlobalObject, value: JSValue) -> JsResult<JSValue> {
    let promise = JsPromise::resolve(value.into_engine(), cx(global));
    global
        .lift(promise)
        .map(|promise| JSValue::from_engine(promise.into()))
}

/// `JSPromise::rejected_promise`.
pub(crate) fn rejected(global: &JSGlobalObject, error: JSValue) -> JsResult<JSValue> {
    let error = boa_engine::JsError::from_opaque(error.into_engine());
    let promise = JsPromise::reject(error, cx(global));
    global
        .lift(promise)
        .map(|promise| JSValue::from_engine(promise.into()))
}

/// `JSPromise::wrap`: what `body` returns, or the exception it throws, as a promise. What script cannot catch (a
/// termination, a runtime limit) is not a rejection: it goes on unwinding.
pub(crate) fn wrap(
    global: &JSGlobalObject,
    body: impl FnOnce() -> JsResult<JSValue>,
) -> JsResult<JSValue> {
    match body() {
        Ok(value) => resolved(global, value),
        Err(JsError::Thrown) => {
            let exception = global.peek_exception();
            match exception {
                Some(error) if global.clear_catchable_exception() => rejected(global, error),
                _ => Err(JsError::Thrown),
            }
        }
        Err(error) => Err(error),
    }
}

fn aligned(bytes: &[u8]) -> AlignedVec<u8> {
    AlignedVec::from_slice(0, bytes)
}

/// A new `ArrayBuffer` that holds a copy of `bytes`.
pub(crate) fn array_buffer(global: &JSGlobalObject, bytes: &[u8]) -> JsResult<JSValue> {
    let buffer = JsArrayBuffer::from_byte_block(aligned(bytes), cx(global));
    global
        .lift(buffer)
        .map(|buffer| JSValue::from_engine(buffer.into()))
}

/// A new `Uint8Array` over a new `ArrayBuffer` that holds a copy of `bytes`.
pub(crate) fn uint8_array(global: &JSGlobalObject, bytes: &[u8]) -> JsResult<JSValue> {
    let buffer = JsArrayBuffer::from_byte_block(aligned(bytes), cx(global));
    let buffer = global.lift(buffer)?;
    let array = JsUint8Array::from_array_buffer(buffer, cx(global));
    global
        .lift(array)
        .map(|array| JSValue::from_engine(array.into()))
}

/// The bytes an `ArrayBuffer`, a `SharedArrayBuffer`, a typed array or a `DataView` views, as
/// `JSValue::as_array_buffer`; empty when detached. `None` for anything else.
pub(crate) fn buffer_source_bytes(global: &JSGlobalObject, value: &JSValue) -> Option<Vec<u8>> {
    let object = value.as_engine().as_object()?;
    if let Ok(buffer) = JsArrayBuffer::from_object(object.clone()) {
        return Some(buffer.data().map(|data| data.to_vec()).unwrap_or_default());
    }
    if let Ok(buffer) = JsSharedArrayBuffer::from_object(object.clone()) {
        return Some(buffer.to_vec());
    }
    let (buffer, offset, length) = if let Ok(array) = JsTypedArray::from_object(object.clone()) {
        let context = cx(global);
        (
            array.buffer(context).ok()?,
            array.byte_offset(context).unwrap_or(0),
            array.byte_length(context).unwrap_or(0),
        )
    } else if let Ok(view) = JsDataView::from_object(object.clone()) {
        let context = cx(global);
        let Ok(buffer) = view.buffer(context) else {
            return Some(Vec::new());
        };
        (
            buffer,
            view.byte_offset(context).unwrap_or(0) as usize,
            view.byte_length(context).unwrap_or(0) as usize,
        )
    } else {
        return None;
    };
    let window = |data: &[u8]| {
        data.get(offset..offset.saturating_add(length))
            .map(<[u8]>::to_vec)
            .unwrap_or_default()
    };
    let buffer = buffer.as_object()?;
    if let Ok(buffer) = JsArrayBuffer::from_object(buffer.clone()) {
        return Some(buffer.data().map(|data| window(&data[..])).unwrap_or_default());
    }
    JsSharedArrayBuffer::from_object(buffer.clone())
        .ok()
        .map(|buffer| window(&buffer.to_vec()))
}

/// The bytes of a `Uint8Array` (a `Buffer` too), a `DataView` or an `ArrayBuffer`: the buffers a path can be.
pub(crate) fn path_buffer_bytes(global: &JSGlobalObject, value: &JSValue) -> Option<Vec<u8>> {
    let object = value.as_engine().as_object()?;
    let is_path_buffer = JsUint8Array::from_object(object.clone()).is_ok()
        || JsDataView::from_object(object.clone()).is_ok()
        || JsArrayBuffer::from_object(object.clone()).is_ok();
    if !is_path_buffer {
        return None;
    }
    buffer_source_bytes(global, value)
}

/// The string a `String` object holds, as JavaScriptCore's `StringObject`.
pub(crate) fn string_object_value(value: &JSValue) -> Option<JSValue> {
    let object = value.as_engine().as_object()?;
    let string = object.downcast_ref::<boa_engine::JsString>()?.clone();
    Some(JSValue::from_engine(string.into()))
}

/// `target[Symbol.hasInstance] = f`, for a constructor whose `instanceof` is not its prototype chain's.
pub(crate) fn define_has_instance(
    global: &JSGlobalObject,
    target: &JSValue,
    f: bun_js::JSHostFn,
) -> JsResult<()> {
    define_symbol_method(global, target, boa_engine::JsSymbol::has_instance(), f, 1, false)
}

/// Makes `serializer` the structured clone steps of the realm's platform objects (`structuredClone`, `postMessage`).
pub(crate) fn set_host_serializer(
    global: &JSGlobalObject,
    serializer: boa_wintertc::store::HostSerializer,
) {
    boa_wintertc::store::set_host_serializer(cx(global), serializer);
}

/// `target[Symbol.for(key)] = f`, writable and not enumerable, as a method of a class's prototype is.
pub(crate) fn define_symbol_for_method(
    global: &JSGlobalObject,
    target: &JSValue,
    key: &str,
    f: bun_js::JSHostFn,
) -> JsResult<()> {
    let symbol_for = cx(global)
        .intrinsics()
        .constructors()
        .symbol()
        .constructor()
        .get(js_string!("for"), cx(global));
    let symbol_for = JSValue::from_engine(global.lift(symbol_for)?);
    let symbol = symbol_for.call(global, JSValue::UNDEFINED, &[JSValue::from_utf8(key.as_bytes())])?;
    let Some(symbol) = symbol.as_engine().as_symbol() else {
        return Ok(());
    };
    define_symbol_method(global, target, symbol, f, 0, true)
}

fn define_symbol_method(
    global: &JSGlobalObject,
    target: &JSValue,
    symbol: boa_engine::JsSymbol,
    f: bun_js::JSHostFn,
    length: usize,
    writable: bool,
) -> JsResult<()> {
    let name = format!("[{}]", symbol.description().map(|d| d.to_std_string_escaped()).unwrap_or_default());
    let function = bun_js::JSFunction::create(global, &name, f, length);
    define_symbol_value(global, target, symbol, function, writable)
}

/// `target[symbol] = value`, not enumerable.
fn define_symbol_value(
    global: &JSGlobalObject,
    target: &JSValue,
    symbol: boa_engine::JsSymbol,
    value: JSValue,
    writable: bool,
) -> JsResult<()> {
    let Some(target) = target.as_engine().as_object() else {
        return Ok(());
    };
    let descriptor = boa_engine::property::PropertyDescriptor::builder()
        .value(value.into_engine())
        .writable(writable)
        .enumerable(false)
        .configurable(true)
        .build();
    let result = target.define_property_or_throw(symbol, descriptor, cx(global));
    global.lift(result).map(drop)
}

/// `target[Symbol.toStringTag] = tag`, read-only.
pub(crate) fn define_to_string_tag(global: &JSGlobalObject, target: &JSValue, tag: &str) -> JsResult<()> {
    let tag = JSValue::from_utf8(tag.as_bytes());
    define_symbol_value(global, target, boa_engine::JsSymbol::to_string_tag(), tag, false)
}

/// `target[Symbol.iterator] = iterator`, as WebIDL's iterable declarations make it.
pub(crate) fn define_iterator(global: &JSGlobalObject, target: &JSValue, iterator: JSValue) -> JsResult<()> {
    define_symbol_value(global, target, boa_engine::JsSymbol::iterator(), iterator, true)
}

/// `target[Symbol.asyncIterator] = iterator`, as WebIDL's async iterable declarations make it.
pub(crate) fn define_async_iterator(global: &JSGlobalObject, target: &JSValue, iterator: JSValue) -> JsResult<()> {
    define_symbol_value(global, target, boa_engine::JsSymbol::async_iterator(), iterator, true)
}

/// Makes `%AsyncIteratorPrototype%` the prototype of `prototype`, as WebIDL does for an async iterator's.
pub(crate) fn inherit_async_iterator_prototype(global: &JSGlobalObject, prototype: &JSValue) {
    let async_iterator = cx(global)
        .intrinsics()
        .objects()
        .iterator_prototypes()
        .async_iterator();
    if let Some(prototype) = prototype.as_engine().as_object() {
        prototype.set_prototype(Some(async_iterator));
    }
}

/// An `ArrayBufferView` as a readable byte stream needs it: its buffer, window, element size and constructor (the
/// realm's own, not what `view.constructor` says).
pub(crate) struct ViewInfo {
    pub buffer: JSValue,
    pub byte_offset: usize,
    pub byte_length: usize,
    pub element_size: usize,
    pub constructor: JSValue,
    /// Elements, for a typed array; bytes, for a `DataView`.
    pub length: usize,
}

pub(crate) fn view_info(global: &JSGlobalObject, value: &JSValue) -> JsResult<Option<ViewInfo>> {
    use boa_engine::builtins::typed_array::TypedArrayKind as K;
    let Some(object) = value.as_engine().as_object() else {
        return Ok(None);
    };
    if let Ok(array) = JsTypedArray::from_object(object.clone()) {
        let constructors = cx(global).intrinsics().constructors();
        let known = match array.kind() {
            Some(K::Int8) => Some((1, constructors.typed_int8_array().constructor())),
            Some(K::Uint8) | None => Some((1, constructors.typed_uint8_array().constructor())),
            Some(K::Uint8Clamped) => Some((1, constructors.typed_uint8clamped_array().constructor())),
            Some(K::Int16) => Some((2, constructors.typed_int16_array().constructor())),
            Some(K::Uint16) => Some((2, constructors.typed_uint16_array().constructor())),
            Some(K::Int32) => Some((4, constructors.typed_int32_array().constructor())),
            Some(K::Uint32) => Some((4, constructors.typed_uint32_array().constructor())),
            Some(K::Float32) => Some((4, constructors.typed_float32_array().constructor())),
            Some(K::Float64) => Some((8, constructors.typed_float64_array().constructor())),
            Some(K::BigInt64) => Some((8, constructors.typed_bigint64_array().constructor())),
            Some(K::BigUint64) => Some((8, constructors.typed_biguint64_array().constructor())),
            #[allow(unreachable_patterns)]
            _ => None,
        };
        // Float16Array, where the engine has it: two bytes, and the constructor of its prototype.
        let (element_size, constructor) = match known {
            Some(known) => known,
            None => {
                let constructor = match object.prototype() {
                    Some(prototype) => global.lift(prototype.get(js_string!("constructor"), cx(global)))?,
                    None => boa_engine::JsValue::undefined(),
                };
                (2, constructor.as_object().unwrap_or_else(|| constructors.typed_uint16_array().constructor()))
            }
        };
        let buffer = global.lift(array.buffer(cx(global)))?;
        let byte_offset = global.lift(array.byte_offset(cx(global)))?;
        let byte_length = global.lift(array.byte_length(cx(global)))?;
        return Ok(Some(ViewInfo {
            buffer: JSValue::from_engine(buffer),
            byte_offset,
            byte_length,
            element_size,
            constructor: JSValue::from_engine(constructor.into()),
            length: byte_length / element_size,
        }));
    }
    if let Ok(view) = JsDataView::from_object(object.clone()) {
        let constructor = cx(global).intrinsics().constructors().data_view().constructor();
        let buffer = global.lift(view.buffer(cx(global)))?;
        let byte_offset = global.lift(view.byte_offset(cx(global)))? as usize;
        let byte_length = global.lift(view.byte_length(cx(global)))? as usize;
        return Ok(Some(ViewInfo {
            buffer: JSValue::from_engine(buffer),
            byte_offset,
            byte_length,
            element_size: 1,
            constructor: JSValue::from_engine(constructor.into()),
            length: byte_length,
        }));
    }
    Ok(None)
}

fn array_buffer_of(buffer: &JSValue) -> Option<JsArrayBuffer> {
    JsArrayBuffer::from_object(buffer.as_engine().as_object()?.clone()).ok()
}

/// `IsDetachedBuffer`.
pub(crate) fn is_detached(buffer: &JSValue) -> bool {
    array_buffer_of(buffer).is_some_and(|buffer| buffer.data().is_none())
}

pub(crate) fn array_buffer_byte_length(buffer: &JSValue) -> usize {
    array_buffer_of(buffer).map_or(0, |buffer| buffer.byte_length())
}

/// `TransferArrayBuffer(buffer)`: a new buffer with the bytes; `buffer` is detached.
pub(crate) fn transfer_array_buffer(global: &JSGlobalObject, buffer: &JSValue) -> JsResult<JSValue> {
    let Some(source) = array_buffer_of(buffer) else {
        return Err(global.throw_type_error(format_args!("Cannot transfer an ArrayBuffer that is not detachable")));
    };
    let data = global.lift(source.detach(&boa_engine::JsValue::undefined()))?;
    let transferred = global.lift(JsArrayBuffer::from_byte_block(data, cx(global)))?;
    Ok(JSValue::from_engine(transferred.into()))
}

/// `CloneArrayBuffer(buffer, offset, length)`.
pub(crate) fn clone_array_buffer(global: &JSGlobalObject, buffer: &JSValue, offset: usize, length: usize) -> JsResult<JSValue> {
    let bytes = array_buffer_of(buffer)
        .and_then(|buffer| buffer.data().and_then(|data| data.get(offset..offset + length).map(<[u8]>::to_vec)))
        .unwrap_or_default();
    array_buffer(global, &bytes)
}

/// Copies `length` bytes from one buffer into another.
pub(crate) fn copy_buffer_bytes(from: &JSValue, from_offset: usize, to: &JSValue, to_offset: usize, length: usize) {
    let (Some(from), Some(to)) = (array_buffer_of(from), array_buffer_of(to)) else {
        return;
    };
    let bytes = from.data().and_then(|data| data.get(from_offset..from_offset + length).map(<[u8]>::to_vec));
    if let (Some(bytes), Some(mut data)) = (bytes, to.data_mut())
        && let Some(window) = data.get_mut(to_offset..to_offset + length)
    {
        window.copy_from_slice(&bytes);
    }
}

/// `new ArrayBuffer(length)`.
pub(crate) fn new_array_buffer(global: &JSGlobalObject, length: usize) -> JsResult<JSValue> {
    array_buffer(global, &vec![0u8; length])
}

/// `new constructor(buffer, byteOffset, length)`.
pub(crate) fn make_view(
    global: &JSGlobalObject,
    constructor: &JSValue,
    buffer: &JSValue,
    byte_offset: usize,
    length: usize,
) -> JsResult<JSValue> {
    let args = [
        buffer.clone(),
        JSValue::js_number(byte_offset as f64),
        JSValue::js_number(length as f64),
    ];
    construct(global, constructor, &args)
}

pub(crate) fn uint8_array_constructor(global: &JSGlobalObject) -> JSValue {
    let constructor = cx(global).intrinsics().constructors().typed_uint8_array().constructor();
    JSValue::from_engine(constructor.into())
}

/// `GetMethod(value, @@asyncIterator)` or `@@iterator`: `None` where it is `undefined` or `null`, or `value` is.
pub(crate) fn symbol_method(global: &JSGlobalObject, value: &JSValue, which: &str) -> JsResult<Option<JSValue>> {
    if value.is_undefined_or_null() {
        return Ok(None);
    }
    let symbol = match which {
        "asyncIterator" => boa_engine::JsSymbol::async_iterator(),
        _ => boa_engine::JsSymbol::iterator(),
    };
    let object = global.lift(value.as_engine().to_object(cx(global)))?;
    let method = global.lift(object.get(symbol, cx(global)))?;
    if method.is_null_or_undefined() {
        return Ok(None);
    }
    if !method.is_callable() {
        return Err(global.throw_type_error(format_args!("The iterator method is not a function")));
    }
    Ok(Some(JSValue::from_engine(method)))
}

/// `new constructor(...args)`.
pub(crate) fn construct(global: &JSGlobalObject, constructor: &JSValue, args: &[JSValue]) -> JsResult<JSValue> {
    let Some(constructor) = constructor.as_engine().as_object() else {
        return Err(global.throw_type_error(format_args!("not a constructor")));
    };
    let args: Vec<boa_engine::JsValue> = args.iter().map(|arg| arg.as_engine().clone()).collect();
    let result = constructor.construct(&args, None, cx(global));
    global.lift(result).map(|object| JSValue::from_engine(object.into()))
}

/// `Promise.all(promises)` with the realm's own `Promise.all`.
pub(crate) fn promise_all(global: &JSGlobalObject, promises: &[JSValue]) -> JsResult<JSValue> {
    let promise = cx(global).intrinsics().constructors().promise().constructor();
    let all = promise.get(js_string!("all"), cx(global));
    let all = JSValue::from_engine(global.lift(all)?);
    let array = bun_js::JSArray::create(global, promises).to_js();
    all.call(global, JSValue::from_engine(promise.into()), &[array])
}

/// Whether `promise` is a promise that has not settled.
pub(crate) fn is_pending(promise: &JSValue) -> bool {
    promise
        .as_engine()
        .as_object()
        .and_then(|object| JsPromise::from_object(object.clone()).ok())
        .is_some_and(|promise| matches!(promise.state(), boa_engine::builtins::promise::PromiseState::Pending))
}

/// UTF-8 as `TextDecoder` decodes it for `text()`: without a BOM, invalid sequences as U+FFFD.
pub(crate) fn decode_utf8_without_bom(bytes: &[u8]) -> JSValue {
    JSValue::from_utf8(bun_core::strings::without_utf8_bom(bytes))
}

/// A string of UTF-16 code units in little-endian bytes; an odd last byte is dropped.
pub(crate) fn string_from_utf16le(bytes: &[u8]) -> JSValue {
    let units: Vec<u16> = bytes
        .chunks_exact(2)
        .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
        .collect();
    JSValue::from_utf16(&units)
}

/// `JSON.parse(text)` with the realm's own `JSON.parse`, whatever `globalThis.JSON` is now.
pub(crate) fn parse_json(global: &JSGlobalObject, text: JSValue) -> JsResult<JSValue> {
    let json = cx(global).intrinsics().objects().json();
    let parse = json.get(js_string!("parse"), cx(global));
    let parse = JSValue::from_engine(global.lift(parse)?);
    parse.call(global, JSValue::from_engine(json.into()), &[text])
}

/// `value instanceof globalThis[name]`; `false` where there is no such constructor.
pub(crate) fn is_instance_of_global(
    global: &JSGlobalObject,
    value: &JSValue,
    name: &str,
) -> JsResult<bool> {
    if !value.is_object() {
        return Ok(false);
    }
    let Some(constructor) = global.to_js_value().get(global, name)? else {
        return Ok(false);
    };
    if !constructor.is_callable() {
        return Ok(false);
    }
    let result = value
        .as_engine()
        .instance_of(constructor.as_engine(), cx(global));
    global.lift(result)
}

/// Sets the `[[Prototype]]` of an object, for an instance whose `new.target` is a subclass.
pub(crate) fn set_prototype(object: &JSValue, prototype: &JSValue) {
    if let (Some(object), Some(prototype)) = (
        object.as_engine().as_object(),
        prototype.as_engine().as_object(),
    ) {
        object.set_prototype(Some(prototype.clone()));
    }
}

/// `Date` of milliseconds since the epoch.
pub(crate) fn date(global: &JSGlobalObject, ms: f64) -> JsResult<JSValue> {
    let constructor = cx(global).intrinsics().constructors().date().constructor();
    let date = constructor.construct(&[ms.into()], None, cx(global));
    global
        .lift(date)
        .map(|date| JSValue::from_engine(date.into()))
}
