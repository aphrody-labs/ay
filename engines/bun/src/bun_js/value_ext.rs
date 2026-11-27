//! Values that Rust makes for node's APIs: BigInts, the result of `new`, the time of a `Date`.

use boa_engine::JsBigInt;
use boa_engine::object::builtins::JsDate;

use crate::{JSGlobalObject, JSValue, JsResult};

impl JSValue {
    /// A BigInt of `value`.
    pub fn from_i128(value: i128) -> JSValue {
        JSValue(JsBigInt::from(value).into())
    }

    /// `new this(...args)`.
    pub fn construct(&self, global: &JSGlobalObject, args: &[JSValue]) -> JsResult<JSValue> {
        let Some(constructor) = self.0.as_constructor() else {
            return Err(global.throw_type_error(format_args!("{} is not a constructor", self.0.type_of())));
        };
        let result = constructor.construct(JSValue::as_engine_slice(args), None, global.cx());
        global.lift(result).map(|object| JSValue(object.into()))
    }

    /// The time value of a `Date`, in milliseconds since the epoch (`NaN` for an invalid one); `None` for anything
    /// else.
    pub fn as_date_ms(&self, global: &JSGlobalObject) -> JsResult<Option<f64>> {
        let Some(date) = self.0.as_object().and_then(|o| JsDate::from_object(o).ok()) else {
            return Ok(None);
        };
        let time = global.lift(date.get_time(global.cx()))?;
        Ok(Some(time.as_number().unwrap_or(f64::NAN)))
    }
}
