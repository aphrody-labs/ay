//! What bun_js does not wrap yet, from Boa (`bun_js::engine`): BigInts, the bytes of a TypedArray or an ArrayBuffer,
//! a Uint8Array of bytes, an object with a prototype, own properties as JavaScriptCore's `getOwnPropertySlot` reads
//! them. Each function uses the context for one engine operation at a time, as bun_js does.

use bun_js::engine::builtins::object::OrdinaryObject;
use bun_js::engine::builtins::proxy::Proxy;
use bun_js::engine::object::builtins::{
    AlignedVec, JsArrayBuffer, JsDataView, JsSharedArrayBuffer, JsTypedArray, JsUint8Array,
};
use bun_js::engine::property::{PropertyDescriptor, PropertyKey};
use bun_js::engine::{Context, JsBigInt, JsObject, JsString};
use bun_js::{JSGlobalObject, JSObject, JSValue, JsResult};

#[allow(clippy::mut_from_ref)]
fn cx(global: &JSGlobalObject) -> &mut Context {
    // SAFETY: every caller here uses it for one engine operation and drops it before anything else reaches
    // `global`; a native function Boa enters meanwhile derives its own from Boa's reborrow.
    unsafe { global.context() }
}

fn lift<T>(global: &JSGlobalObject, result: bun_js::engine::JsResult<T>) -> JsResult<T> {
    result.map_err(|error| global.throw_engine_error(error))
}

pub fn bigint(value: i64) -> JSValue {
    JSValue::from_engine(JsBigInt::new(value).into())
}

/// The decimal digits of a BigInt outside `i64`. `None` for one within it, and for anything else.
pub fn bigint_out_of_i64(value: &JSValue) -> Option<String> {
    let bigint = value.as_engine().as_bigint()?;
    // Two's complement in as few bytes as hold it.
    (bigint.as_inner().to_signed_bytes_le().len() > 8).then(|| bigint.to_string_radix(10))
}

/// A copy of `bytes` in a new Uint8Array.
pub fn uint8_array(global: &JSGlobalObject, bytes: &[u8]) -> JsResult<JSValue> {
    let block = AlignedVec::from_iter(0, bytes.iter().copied());
    let buffer = JsArrayBuffer::from_byte_block(block, cx(global));
    let buffer = lift(global, buffer)?;
    let array = JsUint8Array::from_array_buffer(buffer, cx(global));
    Ok(JSValue::from_engine(lift(global, array)?.into()))
}

/// What an ArrayBufferView or an ArrayBuffer views.
pub enum Bytes {
    Bytes(Vec<u8>),
    Detached,
    /// Neither.
    Other,
}

/// The bytes of a TypedArray, a DataView, an ArrayBuffer or a SharedArrayBuffer (a copy). `views_only` leaves out
/// the buffers.
pub fn bytes_of(global: &JSGlobalObject, value: &JSValue, views_only: bool) -> JsResult<Bytes> {
    let Some(object) = value.as_engine().as_object() else {
        return Ok(Bytes::Other);
    };
    let view = if let Ok(array) = JsTypedArray::from_object(object.clone()) {
        let buffer = array.buffer(cx(global));
        let buffer = lift(global, buffer)?;
        let offset = array.byte_offset(cx(global));
        let offset = lift(global, offset)?;
        let length = array.byte_length(cx(global));
        let length = lift(global, length)?;
        Some((buffer, offset, length))
    } else if let Ok(view) = JsDataView::from_object(object.clone()) {
        let buffer = view.buffer(cx(global));
        let buffer = lift(global, buffer)?;
        let offset = view.byte_offset(cx(global));
        let offset = lift(global, offset)? as usize;
        let length = view.byte_length(cx(global));
        let length = lift(global, length)? as usize;
        Some((buffer, offset, length))
    } else {
        None
    };
    let (buffer, offset, length) = match view {
        Some(view) => view,
        None if views_only => return Ok(Bytes::Other),
        None => (object.clone().into(), 0, usize::MAX),
    };
    let Some(buffer) = buffer.as_object() else {
        return Ok(Bytes::Detached);
    };
    let whole = if let Ok(buffer) = JsArrayBuffer::from_object(buffer.clone()) {
        match buffer.to_vec() {
            Some(bytes) => bytes,
            None => return Ok(Bytes::Detached),
        }
    } else if let Ok(buffer) = JsSharedArrayBuffer::from_object(buffer.clone()) {
        buffer.to_vec()
    } else {
        return Ok(Bytes::Other);
    };
    let start = offset.min(whole.len());
    let end = start.saturating_add(length).min(whole.len());
    Ok(Bytes::Bytes(whole[start..end].to_vec()))
}

/// An ordinary object whose prototype is `prototype`, or `Object.prototype`.
pub fn object_with_prototype(global: &JSGlobalObject, prototype: Option<&JSValue>) -> JSObject {
    let prototype = match prototype.and_then(|p| p.as_engine().as_object()) {
        Some(prototype) => prototype.clone(),
        None => cx(global).intrinsics().constructors().object().prototype(),
    };
    JSObject::from_engine(JsObject::from_proto_and_data(prototype, OrdinaryObject))
}

/// `Object`, the realm's own.
pub fn object_constructor(global: &JSGlobalObject) -> JSValue {
    JSValue::from_engine(
        cx(global)
            .intrinsics()
            .constructors()
            .object()
            .constructor()
            .into(),
    )
}

/// A property key, which is an index where `key` is one.
pub fn key(name: &str) -> PropertyKey {
    PropertyKey::from(JsString::from(name))
}

/// `object[key] = value` as an own data property (JavaScriptCore's `putDirect`).
pub fn put(global: &JSGlobalObject, object: &JSObject, key: &PropertyKey, value: JSValue) {
    let _ = object
        .as_engine()
        .create_data_property(key.clone(), value.into_engine(), cx(global));
}

/// Defines a data property with these attributes.
pub fn define(
    global: &JSGlobalObject,
    object: &JSObject,
    name: &str,
    value: JSValue,
    writable: bool,
    enumerable: bool,
    configurable: bool,
) -> JsResult<()> {
    let descriptor = PropertyDescriptor::builder()
        .value(value.into_engine())
        .writable(writable)
        .enumerable(enumerable)
        .configurable(configurable);
    let result = object
        .as_engine()
        .define_property_or_throw(key(name), descriptor, cx(global));
    lift(global, result).map(drop)
}

/// The own property `key`, read as JavaScriptCore's `getOwnPropertySlot` reads it (a getter runs, a proxy's traps
/// run); `None` where there is none.
pub fn own_property(
    global: &JSGlobalObject,
    object: &JSObject,
    key: PropertyKey,
) -> JsResult<Option<JSValue>> {
    let has = object.as_engine().has_own_property(key.clone(), cx(global));
    if !lift(global, has)? {
        return Ok(None);
    }
    let value = object.as_engine().get(key, cx(global));
    lift(global, value).map(|value| Some(JSValue::from_engine(value)))
}

/// An object of named parameters: a plain object (a class instance too) or a proxy, not a Date, a Map or a Promise.
pub fn holds_named_parameters(object: &JSObject) -> bool {
    let object = object.as_engine();
    object.is::<OrdinaryObject>() || object.is::<Proxy>()
}

pub fn is_constructor(value: &JSValue) -> bool {
    value
        .as_engine()
        .as_object()
        .is_some_and(|object| object.is_constructor())
}

/// Makes `prototype` the prototype of `object` where it derives from the one `object` has (`Buffer.prototype` from
/// `Uint8Array.prototype`).
pub fn derive_prototype(object: &JSValue, prototype: &JSValue) {
    let (Some(object), Some(prototype)) = (
        object.as_engine().as_object(),
        prototype.as_engine().as_object(),
    ) else {
        return;
    };
    let derives = match (prototype.prototype(), object.prototype()) {
        (Some(base), Some(current)) => JsObject::equals(&base, &current),
        _ => false,
    };
    if derives {
        let _ = object.set_prototype(Some(prototype.clone()));
    }
}
