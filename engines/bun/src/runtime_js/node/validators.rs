//! Node's argument validators, as src/runtime/node/util/validators.rs: a value of the wrong type throws
//! `ERR_INVALID_ARG_TYPE`, `The "name" property must be of type string, got number`.

use core::fmt;

use bun_js::JSValue;
use bun_js::bindgen::{self, JsHeap, from_js::invalid_arg_type};

/// A string, or a `String` object.
pub(crate) fn validate_string<'a>(
    heap: &JsHeap<'a>,
    value: &JSValue,
    name: impl fmt::Display,
) -> bindgen::Result<'a, ()> {
    if !value.is_string() && !value.is_string_object() {
        return Err(invalid_arg_type(heap, name, "string", value));
    }
    Ok(())
}

/// An object, a function included, that is neither `null` nor an array.
pub(crate) fn validate_object<'a>(
    heap: &JsHeap<'a>,
    value: &JSValue,
    name: impl fmt::Display,
) -> bindgen::Result<'a, ()> {
    if value.is_null() || value.is_array() || !value.is_object() {
        return Err(invalid_arg_type(heap, name, "object", value));
    }
    Ok(())
}
