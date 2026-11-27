//! `JSValue`: a JavaScript value. The names and signatures follow `bun_jsc::JSValue`; it takes `&self` where that
//! takes `self`, which a call site does not see.

use boa_engine::object::builtins::JsArray;
use boa_engine::property::PropertyKey;
use boa_engine::{JsString, JsValue, js_string};
use boa_gc::{Finalize, Trace};

use crate::{JSGlobalObject, JsResult};

/// A JavaScript value. Rust holding one keeps what it references alive; a class payload that holds one traces it.
#[derive(Clone, Debug, Default, Trace, Finalize)]
// Its `Finalize` does nothing: without the `Drop` that would call it, the value moves out of a `JSValue`.
#[boa_gc(unsafe_no_drop)]
#[repr(transparent)]
pub struct JSValue(pub(crate) JsValue);

/// A property name as `bun_jsc` takes one: bytes, read as UTF-8.
pub(crate) fn key(bytes: &[u8]) -> PropertyKey {
    JsString::from(String::from_utf8_lossy(bytes).as_ref()).into()
}

impl JSValue {
    pub const UNDEFINED: JSValue = JSValue(JsValue::undefined());
    pub const NULL: JSValue = JSValue(JsValue::null());

    /// Boa's value.
    pub fn from_engine(value: JsValue) -> JSValue {
        JSValue(value)
    }

    pub fn as_engine(&self) -> &JsValue {
        &self.0
    }

    pub fn into_engine(self) -> JsValue {
        self.0
    }

    // ───────────────────────────── making values ─────────────────────────────

    pub fn js_boolean(value: bool) -> JSValue {
        JSValue(value.into())
    }

    pub fn js_number(value: f64) -> JSValue {
        JSValue(value.into())
    }

    pub fn js_number_from_int32(value: i32) -> JSValue {
        JSValue(value.into())
    }

    /// The nearest double, as `bun_jsc` does for one past 2^53.
    pub fn js_number_from_uint64(value: u64) -> JSValue {
        JSValue((value as f64).into())
    }

    pub fn js_empty_string(_: &JSGlobalObject) -> JSValue {
        JSValue(js_string!().into())
    }

    /// A string of the UTF-8 in `bytes`, with U+FFFD for each invalid sequence.
    pub fn from_utf8(bytes: &[u8]) -> JSValue {
        JSValue(JsString::from(String::from_utf8_lossy(bytes).as_ref()).into())
    }

    pub fn from_utf16(units: &[u16]) -> JSValue {
        JSValue(JsString::from(units).into())
    }

    /// `{}`; `len` is how many properties it will have, which Boa does not use.
    pub fn create_empty_object(global: &JSGlobalObject, _len: usize) -> JSValue {
        crate::JSObject::create(global).to_js()
    }

    /// An array of `len` holes.
    pub fn create_empty_array(global: &JSGlobalObject, len: usize) -> JsResult<JSValue> {
        let array = JsArray::new(global.cx()).map_err(|error| global.throw_engine_error(error))?;
        if len > 0 {
            let result = array.set(js_string!("length"), len, true, global.cx());
            global.lift(result)?;
        }
        Ok(JSValue(array.into()))
    }

    pub fn create_array_from_slice(
        global: &JSGlobalObject,
        values: &[JSValue],
    ) -> JsResult<JSValue> {
        let array = JsArray::from_iter(values.iter().map(|value| value.0.clone()), global.cx());
        Ok(JSValue(array.into()))
    }

    // ───────────────────────────── kinds ─────────────────────────────

    pub fn is_undefined(&self) -> bool {
        self.0.is_undefined()
    }

    pub fn is_null(&self) -> bool {
        self.0.is_null()
    }

    pub fn is_undefined_or_null(&self) -> bool {
        self.0.is_null_or_undefined()
    }

    pub fn is_boolean(&self) -> bool {
        self.0.is_boolean()
    }

    pub fn is_number(&self) -> bool {
        self.0.is_number()
    }

    pub fn is_int32(&self) -> bool {
        self.0
            .as_number()
            .is_some_and(|n| n == f64::from(n as i32) && !(n == 0.0 && n.is_sign_negative()))
    }

    pub fn is_string(&self) -> bool {
        self.0.is_string()
    }

    /// A `String` object: `new String("a")`, or an instance of a class that extends `String`.
    pub fn is_string_object(&self) -> bool {
        self.0
            .as_object()
            .is_some_and(|object| object.is::<JsString>())
    }

    pub fn is_symbol(&self) -> bool {
        self.0.is_symbol()
    }

    pub fn is_big_int(&self) -> bool {
        self.0.is_bigint()
    }

    pub fn is_object(&self) -> bool {
        self.0.is_object()
    }

    /// A value the GC owns: an object, a string, a symbol or a BigInt.
    pub fn is_cell(&self) -> bool {
        self.is_object() || self.is_string() || self.is_symbol() || self.is_big_int()
    }

    pub fn is_callable(&self) -> bool {
        self.0.is_callable()
    }

    pub fn is_array(&self) -> bool {
        self.0.as_object().is_some_and(|object| object.is_array())
    }

    /// `Array.isArray`: an array, or a proxy of one. A revoked proxy throws, as `JSC::isArray` does.
    pub fn is_array_abstract(&self, global: &JSGlobalObject) -> JsResult<bool> {
        if self.is_array() {
            return Ok(true);
        }
        let Some(object) = self.0.as_object() else {
            return Ok(false);
        };
        if !object.is::<boa_engine::builtins::proxy::Proxy>() {
            return Ok(false);
        }
        let array = global.cx().intrinsics().constructors().array().constructor();
        let is_array = array.get(js_string!("isArray"), global.cx());
        let is_array = global.lift(is_array)?;
        let Some(is_array) = is_array.as_callable() else {
            return Ok(false);
        };
        let result = is_array.call(&array.into(), &[self.0.clone()], global.cx());
        global.lift(result).map(|value| value.to_boolean())
    }

    /// A typed array or a `DataView`: JavaScriptCore's `JSArrayBufferView`.
    pub fn is_array_buffer_view(&self) -> bool {
        use boa_engine::object::builtins::{JsDataView, JsTypedArray};
        self.0.as_object().is_some_and(|object| {
            JsTypedArray::from_object(object.clone()).is_ok() || JsDataView::from_object(object.clone()).is_ok()
        })
    }

    /// A typed array's length in elements, a `DataView`'s in bytes, an `ArrayBuffer`'s byte length; `None` for
    /// anything else.
    pub fn array_buffer_length(&self, global: &JSGlobalObject) -> JsResult<Option<u64>> {
        use boa_engine::object::builtins::{JsArrayBuffer, JsDataView, JsTypedArray};
        let Some(object) = self.0.as_object() else {
            return Ok(None);
        };
        if let Ok(array) = JsTypedArray::from_object(object.clone()) {
            let length = array.length(global.cx());
            return global.lift(length).map(|n| Some(n as u64));
        }
        if let Ok(view) = JsDataView::from_object(object.clone()) {
            let length = view.byte_length(global.cx());
            return global.lift(length).map(|n| Some(n as u64));
        }
        if let Ok(buffer) = JsArrayBuffer::from_object(object.clone()) {
            return Ok(Some(buffer.data().map_or(0, |data| data.len() as u64)));
        }
        Ok(None)
    }

    /// A symbol's description, `None` for one without, or for what is no symbol.
    pub fn symbol_description(&self) -> Option<String> {
        self.0
            .as_symbol()
            .and_then(|symbol| symbol.description())
            .map(|description| description.to_std_string_escaped())
    }

    pub fn is_error(&self) -> bool {
        self.0
            .as_object()
            .is_some_and(|object| object.is::<boa_engine::builtins::error::Error>())
    }

    pub fn is_truthy(&self) -> bool {
        self.0.to_boolean()
    }

    pub fn is_falsey(&self) -> bool {
        !self.is_truthy()
    }

    /// `typeof`.
    pub fn js_type_string(&self, _: &JSGlobalObject) -> &'static str {
        self.0.type_of()
    }

    /// What Node's validators say a value is, as `bun_jsc`'s `type_name`: `typeof`, but `array` for an array.
    pub fn type_name(&self, global: &JSGlobalObject) -> &'static str {
        if self.is_array() {
            "array"
        } else {
            self.js_type_string(global)
        }
    }

    // ───────────────────────────── reading values ─────────────────────────────

    /// `ToBoolean`.
    pub fn to_boolean(&self) -> bool {
        self.0.to_boolean()
    }

    pub fn as_boolean(&self) -> bool {
        debug_assert!(self.is_boolean());
        self.0.as_boolean().unwrap_or(false)
    }

    /// The number, if it is one.
    pub fn get_number(&self) -> Option<f64> {
        self.0.as_number()
    }

    /// As `bun_jsc`: a number as itself, `undefined` and `null` as 0, a boolean as 0 or 1, anything else NaN.
    pub fn as_number(&self) -> f64 {
        if let Some(number) = self.0.as_number() {
            return number;
        }
        if let Some(boolean) = self.0.as_boolean() {
            return f64::from(u8::from(boolean));
        }
        if self.is_undefined_or_null() {
            0.0
        } else {
            f64::NAN
        }
    }

    /// An int32 (see [`is_int32`](Self::is_int32)).
    pub fn as_int32(&self) -> i32 {
        debug_assert!(self.is_int32());
        self.get_number().map_or(0, to_int32)
    }

    /// A number truncated to an integer and clamped to `u32`; 0 for anything else.
    pub fn to_u32(&self) -> u32 {
        match self.get_number() {
            Some(number) if !number.is_nan() => {
                (number as i64).clamp(0, i64::from(u32::MAX)) as u32
            }
            _ => 0,
        }
    }

    /// `ToNumber`.
    pub fn to_number(&self, global: &JSGlobalObject) -> JsResult<f64> {
        let result = self.0.to_number(global.cx());
        global.lift(result)
    }

    /// `ToNumber`, as `bun_jsc` names it where it may coerce.
    pub fn coerce_to_f64(&self, global: &JSGlobalObject) -> JsResult<f64> {
        self.to_number(global)
    }

    /// `ToInt32`.
    pub fn coerce_to_i32(&self, global: &JSGlobalObject) -> JsResult<i32> {
        let result = self.0.to_i32(global.cx());
        global.lift(result)
    }

    /// `ToString`, as UTF-8 (lone surrogates become U+FFFD).
    pub fn to_utf8(&self, global: &JSGlobalObject) -> JsResult<String> {
        let result = self.0.to_string(global.cx());
        global
            .lift(result)
            .map(|string| string.to_std_string_lossy())
    }

    /// `ToString`, as a string value.
    pub fn to_js_string(&self, global: &JSGlobalObject) -> JsResult<JSValue> {
        let result = self.0.to_string(global.cx());
        global.lift(result).map(|string| JSValue(string.into()))
    }

    /// `ToString`, as UTF-16.
    pub fn to_utf16(&self, global: &JSGlobalObject) -> JsResult<Vec<u16>> {
        let result = self.0.to_string(global.cx());
        global.lift(result).map(|string| string.iter().collect())
    }

    /// A string as UTF-8, without coercing anything else.
    pub fn as_string(&self) -> Option<String> {
        self.0
            .as_string()
            .map(|string| string.to_std_string_lossy())
    }

    pub fn get_object(&self) -> Option<crate::JSObject> {
        self.0.as_object().map(crate::JSObject)
    }

    // ───────────────────────────── properties ─────────────────────────────

    /// `value[property]`, `None` where it is `undefined` or `value` is not an object.
    pub fn get(
        &self,
        global: &JSGlobalObject,
        property: impl AsRef<[u8]>,
    ) -> JsResult<Option<JSValue>> {
        let Some(object) = self.0.as_object() else {
            return Ok(None);
        };
        let result = object.get(key(property.as_ref()), global.cx());
        let value = global.lift(result)?;
        Ok((!value.is_undefined()).then_some(JSValue(value)))
    }

    /// `value[property]` as a `T`, `None` where it is `undefined` or `null`.
    pub fn get_optional<T: CoerceTo>(
        &self,
        global: &JSGlobalObject,
        property: impl AsRef<[u8]>,
    ) -> JsResult<Option<T>> {
        match self.get(global, property)? {
            Some(value) if !value.is_undefined_or_null() => T::coerce_from(value, global).map(Some),
            _ => Ok(None),
        }
    }

    pub fn get_index(&self, global: &JSGlobalObject, index: u32) -> JsResult<JSValue> {
        match self.get_object() {
            Some(object) => object.get_index(global, index),
            None => Ok(JSValue::UNDEFINED),
        }
    }

    /// Defines `value[key]` as an own data property, as JavaScriptCore's `putDirect`: no setter runs, a prototype is
    /// not consulted. `value` is an object.
    pub fn put(&self, global: &JSGlobalObject, key: impl AsRef<[u8]>, property: JSValue) {
        match self.0.as_object() {
            Some(object) => {
                let _ =
                    object.create_data_property(self::key(key.as_ref()), property.0, global.cx());
            }
            None => debug_assert!(false, "put on a {}", self.0.type_of()),
        }
    }

    /// `value.length` of an object, clamped to `0..2^51`; a string's length; 0 for anything else.
    pub fn get_length(&self, global: &JSGlobalObject) -> JsResult<u64> {
        if let Some(string) = self.0.as_string() {
            return Ok(string.len() as u64);
        }
        let Some(length) = self.get(global, "length")? else {
            return Ok(0);
        };
        let length = length.to_number(global)?;
        const I52_MAX: f64 = ((1u64 << 51) - 1) as f64;
        Ok(if length.is_nan() {
            0
        } else {
            length.clamp(0.0, I52_MAX) as u64
        })
    }

    // ───────────────────────────── calls and comparisons ─────────────────────────────

    /// Calls the function with `this` and `args`.
    pub fn call(
        &self,
        global: &JSGlobalObject,
        this: JSValue,
        args: &[JSValue],
    ) -> JsResult<JSValue> {
        let Some(function) = self.0.as_callable() else {
            return Err(
                global.throw_type_error(format_args!("{} is not a function", self.0.type_of()))
            );
        };
        let result = function.call(&this.0, JSValue::as_engine_slice(args), global.cx());
        global.lift(result).map(JSValue)
    }

    /// `Object.is`.
    pub fn is_same_value(&self, other: JSValue, _: &JSGlobalObject) -> JsResult<bool> {
        Ok(JsValue::same_value(&self.0, &other.0))
    }

    /// `===`.
    pub fn is_strict_equal(&self, other: JSValue, _: &JSGlobalObject) -> JsResult<bool> {
        Ok(self.0.strict_equals(&other.0))
    }

    pub(crate) fn as_engine_slice(values: &[JSValue]) -> &[JsValue] {
        // SAFETY: `JSValue` is `repr(transparent)` over `JsValue`.
        unsafe { &*(core::ptr::from_ref(values) as *const [JsValue]) }
    }

    pub(crate) fn from_engine_slice(values: &[JsValue]) -> &[JSValue] {
        // SAFETY: as above.
        unsafe { &*(core::ptr::from_ref(values) as *const [JSValue]) }
    }
}

/// What [`JSValue::get_optional`] converts a property to.
pub trait CoerceTo: Sized {
    fn coerce_from(value: JSValue, global: &JSGlobalObject) -> JsResult<Self>;
}

impl CoerceTo for JSValue {
    fn coerce_from(value: JSValue, _: &JSGlobalObject) -> JsResult<JSValue> {
        Ok(value)
    }
}

/// A number truncated (NaN is 0, out of range saturates), anything else `ToInt32`, as in `bun_jsc`.
impl CoerceTo for i32 {
    fn coerce_from(value: JSValue, global: &JSGlobalObject) -> JsResult<i32> {
        match value.get_number() {
            Some(number) => Ok(if number.is_nan() { 0 } else { number as i32 }),
            None => value.coerce_to_i32(global),
        }
    }
}

/// A number truncated (NaN is 0, out of range saturates), a BigInt as `BigInt.asIntN(64)`, anything else `ToNumber`
/// first, as in `bun_jsc`.
impl CoerceTo for i64 {
    fn coerce_from(value: JSValue, global: &JSGlobalObject) -> JsResult<i64> {
        if let Some(bigint) = value.0.as_bigint() {
            // Two's complement, least significant byte first: the low 8 bytes are the value modulo 2^64.
            let bytes = bigint.as_inner().to_signed_bytes_le();
            let fill = if bytes.last().is_some_and(|byte| byte & 0x80 != 0) {
                0xff
            } else {
                0
            };
            let mut low = [fill; 8];
            for (to, from) in low.iter_mut().zip(&bytes) {
                *to = *from;
            }
            return Ok(i64::from_le_bytes(low));
        }
        let number = match value.get_number() {
            Some(number) => number,
            None => value.to_number(global)?,
        };
        Ok(if number.is_nan() { 0 } else { number as i64 })
    }
}

impl From<JsValue> for JSValue {
    fn from(value: JsValue) -> JSValue {
        JSValue(value)
    }
}

impl From<JSValue> for JsValue {
    fn from(value: JSValue) -> JsValue {
        value.0
    }
}

/// `ToInt32` of a number.
pub(crate) fn to_int32(number: f64) -> i32 {
    if !number.is_finite() {
        return 0;
    }
    number.trunc().rem_euclid(4_294_967_296.0) as u32 as i32
}
