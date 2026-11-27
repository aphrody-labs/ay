//! `ErrorCode`: Node's error codes (`ERR_INVALID_ARG_TYPE`, …), each with its constructor, as in `bun_jsc`. As in
//! src/jsc/bindings/ErrorCode.cpp, an error of a code has a prototype of its own, which holds its `name`, its `code`
//! and a `toString` that answers `Name [CODE]: message`.

use core::fmt;

use boa_engine::builtins::object::OrdinaryObject;
use boa_engine::object::FunctionObjectBuilder;
use boa_engine::property::PropertyDescriptor;
use boa_engine::{
    Context, JsObject, JsResult as EngineResult, JsString, JsValue, NativeFunction, js_str,
    js_string,
};

use crate::{JSGlobalObject, JSValue, JsError, JsResult};

/// The constructor of an error.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ErrorKind {
    Error,
    TypeError,
    RangeError,
    SyntaxError,
    URIError,
}

impl ErrorKind {
    fn name(self) -> &'static str {
        match self {
            ErrorKind::Error => "Error",
            ErrorKind::TypeError => "TypeError",
            ErrorKind::RangeError => "RangeError",
            ErrorKind::SyntaxError => "SyntaxError",
            ErrorKind::URIError => "URIError",
        }
    }
}

/// A code of src/jsc/bindings/ErrorCode.ts.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ErrorCode {
    code: &'static str,
    kind: ErrorKind,
    /// `error.name`, where it is not the constructor's.
    name: Option<&'static str>,
}

include!(concat!(env!("OUT_DIR"), "/error_codes.rs"));

fn data(value: JsValue) -> PropertyDescriptor {
    PropertyDescriptor::builder()
        .value(value)
        .writable(true)
        .enumerable(true)
        .configurable(true)
        .build()
}

/// `Name [CODE]: message`.
fn to_string(this: &JsValue, _: &[JsValue], context: &mut Context) -> EngineResult<JsValue> {
    let this = this.to_object(context)?;
    let name = this.get(js_string!("name"), context)?;
    let code = this.get(js_string!("code"), context)?;
    let message = this.get(js_string!("message"), context)?;
    let (name, code, message) = (
        name.to_string(context)?,
        code.to_string(context)?,
        message.to_string(context)?,
    );
    Ok(js_string!(&name, js_str!(" ["), &code, js_str!("]: "), &message).into())
}

impl ErrorCode {
    pub fn code(self) -> &'static str {
        self.code
    }

    pub fn kind(self) -> ErrorKind {
        self.kind
    }

    pub fn name(self) -> &'static str {
        self.name.unwrap_or(self.kind.name())
    }

    fn prototype(self, global: &JSGlobalObject) -> JsResult<JsObject> {
        if let Some(prototype) = global.state().error_prototypes.get(&self) {
            return Ok(prototype.clone());
        }
        let base = {
            let constructors = global.cx().intrinsics().constructors();
            match self.kind {
                ErrorKind::Error => constructors.error(),
                ErrorKind::TypeError => constructors.type_error(),
                ErrorKind::RangeError => constructors.range_error(),
                ErrorKind::SyntaxError => constructors.syntax_error(),
                ErrorKind::URIError => constructors.uri_error(),
            }
            .prototype()
        };
        let prototype = JsObject::from_proto_and_data(Some(base), OrdinaryObject);
        let to_string =
            FunctionObjectBuilder::new(global.cx().realm(), NativeFunction::from_fn_ptr(to_string))
                .name(js_string!("toString"))
                .length(0)
                .build();
        for (key, value) in [
            ("name", JsValue::from(JsString::from(self.name()))),
            ("code", JsString::from(self.code).into()),
            ("toString", to_string.into()),
        ] {
            let result =
                prototype.define_property_or_throw(JsString::from(key), data(value), global.cx());
            global.lift(result)?;
        }
        global
            .state()
            .error_prototypes
            .insert(self, prototype.clone());
        Ok(prototype)
    }

    /// The error: an instance of its constructor whose prototype is the code's.
    pub fn fmt(self, global: &JSGlobalObject, args: fmt::Arguments<'_>) -> JSValue {
        let error = global.create_error_of_kind(self.kind, args);
        // Its prototype is made of data properties on a fresh object: nothing in it runs script or fails.
        if let (Some(object), Ok(prototype)) = (error.0.as_object(), self.prototype(global)) {
            object.set_prototype(Some(prototype));
        }
        error
    }

    /// Throws [`fmt`](Self::fmt).
    pub fn throw(self, global: &JSGlobalObject, args: fmt::Arguments<'_>) -> JsError {
        global.throw_value(self.fmt(global, args))
    }
}
