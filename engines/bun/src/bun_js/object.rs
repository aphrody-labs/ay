//! `JSObject` and `JSArray`.

use boa_engine::builtins::object::OrdinaryObject;
use boa_engine::object::builtins::JsArray;
use boa_engine::property::{Attribute, PropertyDescriptor};
use boa_engine::{JsObject, JsString, JsValue};

use crate::{JSGlobalObject, JSValue, JsResult};

/// An object.
#[derive(Clone, Debug)]
#[repr(transparent)]
pub struct JSObject(pub(crate) JsObject);

impl JSObject {
    /// `{}`.
    pub fn create(global: &JSGlobalObject) -> JSObject {
        JSObject(JsObject::with_object_proto(global.cx().intrinsics()))
    }

    /// An object with these properties, in this order.
    pub fn create_from(
        global: &JSGlobalObject,
        properties: &[(&str, JSValue)],
    ) -> JsResult<JSObject> {
        let object = JSObject::create(global);
        for (key, value) in properties {
            object.put(global, key, value.clone());
        }
        Ok(object)
    }

    pub fn from_engine(object: JsObject) -> JSObject {
        JSObject(object)
    }

    pub fn as_engine(&self) -> &JsObject {
        &self.0
    }

    pub fn to_js(&self) -> JSValue {
        JSValue(self.0.clone().into())
    }

    /// `object[key]`, `undefined` included.
    pub fn get(&self, global: &JSGlobalObject, key: impl AsRef<[u8]>) -> JsResult<JSValue> {
        let result = self.0.get(crate::value::key(key.as_ref()), global.cx());
        global.lift(result).map(JSValue)
    }

    pub fn get_index(&self, global: &JSGlobalObject, index: u32) -> JsResult<JSValue> {
        let result = self.0.get(index, global.cx());
        global.lift(result).map(JSValue)
    }

    /// Defines `object[key]` as an own data property, as JavaScriptCore's `putDirect`: no setter runs, a prototype
    /// is not consulted.
    pub fn put(&self, global: &JSGlobalObject, key: impl AsRef<[u8]>, value: JSValue) {
        let _ = self
            .0
            .create_data_property(crate::value::key(key.as_ref()), value.0, global.cx());
    }

    /// `object[key] = value`, as script assigns: a setter runs, and it throws where it fails, as in strict mode.
    pub fn set(
        &self,
        global: &JSGlobalObject,
        key: impl AsRef<[u8]>,
        value: JSValue,
    ) -> JsResult<()> {
        let result = self
            .0
            .set(crate::value::key(key.as_ref()), value.0, true, global.cx());
        global.lift(result).map(drop)
    }

    pub fn put_index(&self, global: &JSGlobalObject, index: u32, value: JSValue) -> JsResult<()> {
        let result = self.0.set(index, value.0, true, global.cx());
        global.lift(result).map(drop)
    }

    /// A property that is neither enumerable nor writable nor configurable, as `ReadOnly|DontDelete|DontEnum`.
    pub fn put_read_only(
        &self,
        global: &JSGlobalObject,
        key: &str,
        value: JSValue,
    ) -> JsResult<()> {
        self.define(global, key, value, Attribute::empty())
    }

    /// A property that is writable and configurable but not enumerable, as a built-in method is.
    pub fn put_non_enumerable(
        &self,
        global: &JSGlobalObject,
        key: &str,
        value: JSValue,
    ) -> JsResult<()> {
        self.define(
            global,
            key,
            value,
            Attribute::WRITABLE | Attribute::CONFIGURABLE,
        )
    }

    fn define(
        &self,
        global: &JSGlobalObject,
        key: &str,
        value: JSValue,
        attribute: Attribute,
    ) -> JsResult<()> {
        let descriptor = PropertyDescriptor::builder()
            .value(value.0)
            .writable(attribute.writable())
            .enumerable(attribute.enumerable())
            .configurable(attribute.configurable());
        let result = self
            .0
            .define_property_or_throw(JsString::from(key), descriptor, global.cx());
        global.lift(result).map(drop)
    }

    pub fn has_property(&self, global: &JSGlobalObject, key: &str) -> JsResult<bool> {
        let result = self.0.has_property(JsString::from(key), global.cx());
        global.lift(result)
    }

    pub fn delete_property(&self, global: &JSGlobalObject, key: &str) -> JsResult<bool> {
        let result = self
            .0
            .delete_property_or_throw(JsString::from(key), global.cx());
        global.lift(result)
    }

    /// The object's own enumerable string keys, in property order: the built-in `Object.keys`, whatever
    /// `globalThis.Object.keys` is now.
    pub fn keys(&self, global: &JSGlobalObject) -> JsResult<Vec<String>> {
        let result =
            OrdinaryObject::keys(&JsValue::undefined(), &[self.0.clone().into()], global.cx());
        let keys = JSValue(global.lift(result)?);
        let length = keys.get_length(global)?;
        (0..length as u32)
            .map(|index| keys.get_index(global, index)?.to_utf8(global))
            .collect()
    }
}

/// An array.
#[derive(Clone, Debug)]
pub struct JSArray(JsArray);

impl JSArray {
    pub fn create(global: &JSGlobalObject, values: &[JSValue]) -> JSArray {
        JSArray(JsArray::from_iter(
            JSValue::as_engine_slice(values).iter().cloned(),
            global.cx(),
        ))
    }

    pub fn push(&self, global: &JSGlobalObject, value: JSValue) -> JsResult<()> {
        let result = self.0.push(value.0, global.cx());
        global.lift(result).map(drop)
    }

    pub fn len(&self, global: &JSGlobalObject) -> JsResult<u64> {
        let result = self.0.length(global.cx());
        global.lift(result)
    }

    pub fn to_js(&self) -> JSValue {
        JSValue(self.0.clone().into())
    }
}
