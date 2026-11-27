//! Internal fields (`$getInternalField`, `$putInternalField`, a `JSC::InternalFieldTuple`). JavaScriptCore keeps them in
//! the object's cell; here they are a WeakMap from the object to the array of its fields, which the intrinsics of
//! Bun's builtin modules share (`JSGlobalObject::internal_fields`).

use boa_engine::JsValue;
use boa_engine::object::builtins::{JsArray, JsWeakMap};

use crate::{JSGlobalObject, JSObject, JSValue, JsResult};

struct InternalFields(JsWeakMap);

impl JSGlobalObject {
    /// The WeakMap of every object's internal fields in this realm, made the first time.
    pub fn internal_fields(&self) -> JSValue {
        JSValue(self.internal_field_map().into())
    }

    fn internal_field_map(&self) -> JsWeakMap {
        if let Some(InternalFields(map)) = self.cx().host_defined_mut().get::<InternalFields>() {
            return map.clone();
        }
        let map = JsWeakMap::new(self.cx());
        self.cx()
            .host_defined_mut()
            .insert(InternalFields(map.clone()));
        map
    }

    /// The array of `object`'s internal fields, made if `create` and there is none.
    fn internal_fields_of(&self, object: &JSObject, create: bool) -> JsResult<Option<JsArray>> {
        let map = self.internal_field_map();
        let result = map.get(&object.0, self.cx());
        let fields = self.lift(result)?;
        if let Some(fields) = fields
            .as_object()
            .and_then(|o| JsArray::from_object(o.clone()).ok())
        {
            return Ok(Some(fields));
        }
        if !create {
            return Ok(None);
        }
        let fields = JsArray::new(self.cx()).map_err(|error| self.throw_engine_error(error))?;
        let result = map.set(&object.0, JsValue::from(fields.clone()), self.cx());
        self.lift(result)?;
        Ok(Some(fields))
    }
}

impl JSObject {
    /// The internal field `index`, `undefined` if it was never put.
    pub fn get_internal_field(&self, global: &JSGlobalObject, index: u32) -> JsResult<JSValue> {
        let Some(fields) = global.internal_fields_of(self, false)? else {
            return Ok(JSValue::UNDEFINED);
        };
        let result = fields.get(index, global.cx());
        global.lift(result).map(JSValue)
    }

    pub fn put_internal_field(
        &self,
        global: &JSGlobalObject,
        index: u32,
        value: JSValue,
    ) -> JsResult<()> {
        let fields = global
            .internal_fields_of(self, true)?
            .expect("made where there is none");
        let result = fields.set(index, value.0, true, global.cx());
        global.lift(result).map(drop)
    }
}
