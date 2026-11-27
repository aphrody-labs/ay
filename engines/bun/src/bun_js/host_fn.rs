//! Host functions: Rust that JavaScript calls.

use boa_engine::object::FunctionObjectBuilder;
use boa_engine::property::PropertyDescriptor;
use boa_engine::{Context, JsString, JsValue, NativeFunction, js_string};

use crate::{CallFrame, JSGlobalObject, JSValue, JsResult};

/// A function JavaScript calls, as `bun_jsc::JSHostFn`: an exception it throws is pending on `global` when it
/// returns `Err`.
pub type JSHostFn = fn(&JSGlobalObject, &CallFrame<'_>) -> JsResult<JSValue>;

/// Calls `f` as Boa's native function `(this, args, context)`.
pub(crate) fn call_host_fn(
    f: impl FnOnce(&JSGlobalObject, &CallFrame<'_>) -> JsResult<JSValue>,
    this: &JsValue,
    args: &[JsValue],
    context: &mut Context,
) -> boa_engine::JsResult<JsValue> {
    let global = JSGlobalObject::from_context(context);
    let frame = CallFrame::new(this, args);
    match f(global, &frame) {
        Ok(value) => {
            debug_assert!(
                !global.has_exception(),
                "a host function returned a value with an exception pending"
            );
            Ok(value.0)
        }
        Err(error) => Err(global.into_engine_error(error)),
    }
}

/// Boa's native function for a host function.
pub fn native_function(f: JSHostFn) -> NativeFunction {
    NativeFunction::from_copy_closure(move |this, args, context| {
        call_host_fn(f, this, args, context)
    })
}

/// A function object.
pub struct JSFunction;

impl JSFunction {
    /// A function named `name` whose `length` is `length`, which calls `f`.
    pub fn create(global: &JSGlobalObject, name: &str, f: JSHostFn, length: usize) -> JSValue {
        let context = global.cx();
        let function = FunctionObjectBuilder::new(context.realm(), native_function(f))
            .name(JsString::from(name))
            .length(length)
            .build();
        JSValue(function.into())
    }

    /// As [`create`](Self::create), but its `name` property is `bound <name>`, as a bound function's is, while it
    /// prints (`Function.prototype.toString`) and is traced as `name`: JavaScriptCore's `setIsNamedAsBound`.
    pub fn create_named_as_bound(
        global: &JSGlobalObject,
        name: &str,
        f: JSHostFn,
        length: usize,
    ) -> JSValue {
        let function = Self::create(global, name, f, length);
        let object = function.0.as_object().expect("a function is an object");
        object.insert_property(
            js_string!("name"),
            PropertyDescriptor::builder()
                .value(JsString::from(format!("bound {name}").as_str()))
                .writable(false)
                .enumerable(false)
                .configurable(true),
        );
        function
    }

    /// As [`create`](Self::create), as a function that `new` can call too: `frame.this()` is then the `new.target`, as for a class's
    /// constructor.
    pub fn create_constructor(
        global: &JSGlobalObject,
        name: &str,
        f: JSHostFn,
        length: usize,
    ) -> JSValue {
        let context = global.cx();
        let function = FunctionObjectBuilder::new(context.realm(), native_function(f))
            .name(JsString::from(name))
            .length(length)
            .constructor(true)
            .build();
        JSValue(function.into())
    }
}
