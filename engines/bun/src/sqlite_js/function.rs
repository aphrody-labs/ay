//! db.function() and db.aggregate(): JavaScript that SQL calls, as SqliteUserFunction.h, registered on the
//! connection through Turso's extension API. Turso has no window functions, so `inverse` is never called.
//!
//! What a callback throws stays pending: whoever stepped the statement finds it before the engine's error
//! (`SqliteError::throw`).

use std::cell::Cell;
use std::ffi::{CString, c_void};
use std::rc::{Rc, Weak};

use bun_core::heap;
use bun_js::{JSGlobalObject, JSValue, JsResult};
use turso_ext::{AggCtx, ContextDestructor, ValueDestructor, Value as ExtValue};

use crate::binder::{FromJs, from_js};
use crate::connection::Connection;
use crate::error::{SqliteError, errstr};
use crate::statement::{CallbackScope, to_js};

/// `SQLITE_MISUSE`: what sqlite3_create_function_v2() answers for a name or an argument count it refuses.
const SQLITE_MISUSE: i32 = 21;
/// `SQLITE_MAX_FUNCTION_ARG`.
const MAX_ARGUMENTS: i32 = 127;
const MAX_NAME_BYTES: usize = 255;

thread_local! {
    static STEPPING: Cell<*const JSGlobalObject> = const { Cell::new(std::ptr::null()) };
}

/// While it lives, the user functions that the engine calls run in `global`; outside of it they fail.
pub struct Stepping(*const JSGlobalObject);

impl Stepping {
    pub fn enter(global: &JSGlobalObject) -> Stepping {
        Stepping(STEPPING.replace(global))
    }
}

impl Drop for Stepping {
    fn drop(&mut self) {
        STEPPING.set(self.0);
    }
}

fn stepping<'a>() -> Option<&'a JSGlobalObject> {
    // SAFETY: a `Stepping` sets it to the `global` of the host function that steps, which outlives the step.
    unsafe { STEPPING.get().as_ref() }.filter(|global| !global.has_exception())
}

pub enum Kind {
    Scalar(JSValue),
    Aggregate {
        /// The first accumulator of a group; a function is called to make it.
        start: JSValue,
        step: JSValue,
        /// `None`: the accumulator is the result.
        result: Option<JSValue>,
    },
}

struct Callbacks {
    connection: Weak<Connection>,
    name: String,
    safe_integers: bool,
    kind: Kind,
}

impl Callbacks {
    fn call(&self, global: &JSGlobalObject, function: &JSValue, args: &[JSValue]) -> JsResult<JSValue> {
        let connection = self.connection.upgrade();
        let _scope = connection.as_deref().map(CallbackScope::new);
        function.call(global, JSValue::UNDEFINED, args)
    }

    fn arguments(&self, global: &JSGlobalObject, argc: i32, argv: *const ExtValue) -> JsResult<Vec<JSValue>> {
        let args = if argc <= 0 || argv.is_null() {
            &[][..]
        } else {
            // SAFETY: Turso passes `argc` values at `argv`, alive for the call.
            unsafe { std::slice::from_raw_parts(argv, argc as usize) }
        };
        args.iter()
            .map(|arg| {
                let value = turso_core::Value::from_ffi_ref(arg).unwrap_or(turso_core::Value::Null);
                to_js(global, &value, self.safe_integers)
            })
            .collect()
    }

    fn result(&self, global: &JSGlobalObject, value: &JSValue) -> JsResult<ExtValue> {
        match from_js(global, value, self.safe_integers)? {
            FromJs::Value(value) => Ok(value.to_ffi()),
            FromJs::Unsupported => Err(global.throw_type_error(format_args!(
                "User-defined function {}() returned an invalid value",
                self.name
            ))),
        }
    }

    /// The accumulator of a group, made from `start` the first time. If `start` throws, it is `undefined`.
    fn accumulator(&self, global: &JSGlobalObject, accumulator: &mut Accumulator) -> JsResult<JSValue> {
        if let Some(value) = &accumulator.0 {
            return Ok(value.clone());
        }
        accumulator.0 = Some(JSValue::UNDEFINED);
        let Kind::Aggregate { start, .. } = &self.kind else {
            unreachable!("an aggregate's callbacks");
        };
        let first = if start.is_callable() {
            self.call(global, start, &[])?
        } else {
            start.clone()
        };
        accumulator.0 = Some(first.clone());
        Ok(first)
    }
}

/// A failed call: an empty error, the exception pending for the caller of the step.
fn finish(result: JsResult<ExtValue>) -> ExtValue {
    result.unwrap_or_else(|_| ExtValue::error_with_message(String::new()))
}

/// # Safety
/// `context` is a `Callbacks` that `define` registered and `destroy` has not freed.
unsafe fn callbacks<'a>(context: usize) -> &'a Callbacks {
    // SAFETY: the caller's.
    unsafe { &*(context as *const Callbacks) }
}

unsafe extern "C" fn call_scalar(
    context: usize,
    argc: i32,
    argv: *const ExtValue,
    _: Option<ContextDestructor>,
    _: Option<ValueDestructor>,
) -> ExtValue {
    // SAFETY: Turso calls it with the context it was registered with.
    let callbacks = unsafe { callbacks(context) };
    let (Some(global), Kind::Scalar(function)) = (stepping(), &callbacks.kind) else {
        return ExtValue::error_with_message(String::new());
    };
    finish((|| {
        let arguments = callbacks.arguments(global, argc, argv)?;
        let value = callbacks.call(global, function, &arguments)?;
        callbacks.result(global, &value)
    })())
}

unsafe extern "C" fn destroy(context: usize) {
    // SAFETY: Turso calls it once, after the last call.
    unsafe { heap::destroy(context as *mut Callbacks) };
}

/// The state of a group: `None` until `start` made the first accumulator.
struct Accumulator(Option<JSValue>);

unsafe extern "C" fn init_aggregate(_: usize) -> *mut AggCtx {
    let state = heap::into_raw(Box::new(Accumulator(None))).cast::<c_void>();
    heap::into_raw(Box::new(AggCtx { state }))
}

/// # Safety
/// `ctx` is what `init_aggregate` made and `destroy_aggregate` has not freed.
unsafe fn accumulator_of<'a>(ctx: *mut AggCtx) -> &'a mut Accumulator {
    // SAFETY: the caller's.
    unsafe { &mut *(*ctx).state.cast::<Accumulator>() }
}

unsafe extern "C" fn destroy_aggregate(ctx: usize) {
    // SAFETY: Turso calls it once for each `init_aggregate`, after the group's last call.
    unsafe {
        let ctx = heap::take(ctx as *mut AggCtx);
        heap::destroy(ctx.state.cast::<Accumulator>());
    }
}

unsafe extern "C" fn step_aggregate(context: usize, ctx: *mut AggCtx, argc: i32, argv: *const ExtValue) -> ExtValue {
    // SAFETY: Turso calls it with the context it was registered with and a group's state.
    let (callbacks, accumulator) = unsafe { (callbacks(context), accumulator_of(ctx)) };
    let (Some(global), Kind::Aggregate { step, .. }) = (stepping(), &callbacks.kind) else {
        return ExtValue::error_with_message(String::new());
    };
    finish((|| {
        let mut arguments = vec![callbacks.accumulator(global, accumulator)?];
        arguments.extend(callbacks.arguments(global, argc, argv)?);
        let next = callbacks.call(global, step, &arguments)?;
        // `undefined` keeps the accumulator.
        if !next.is_undefined() {
            accumulator.0 = Some(next);
        }
        Ok(ExtValue::null())
    })())
}

unsafe extern "C" fn finalize_aggregate(context: usize, ctx: *mut AggCtx) -> ExtValue {
    // SAFETY: as `step_aggregate`.
    let (callbacks, accumulator) = unsafe { (callbacks(context), accumulator_of(ctx)) };
    let (Some(global), Kind::Aggregate { result, .. }) = (stepping(), &callbacks.kind) else {
        return ExtValue::error_with_message(String::new());
    };
    finish((|| {
        let value = callbacks.accumulator(global, accumulator)?;
        let value = match result {
            Some(result) => callbacks.call(global, result, &[value])?,
            None => value,
        };
        callbacks.result(global, &value)
    })())
}

/// `sqlite3_create_function_v2` / `sqlite3_create_window_function` on `connection`. A function of the same name and
/// argument count is replaced.
pub fn define(
    connection: &Rc<Connection>,
    conn: &turso_core::Connection,
    name: &str,
    argument_count: i32,
    deterministic: bool,
    safe_integers: bool,
    kind: Kind,
) -> Result<(), SqliteError> {
    if !(-1..=MAX_ARGUMENTS).contains(&argument_count) || name.len() > MAX_NAME_BYTES {
        return Err(SqliteError::new(SQLITE_MISUSE, errstr(SQLITE_MISUSE)));
    }
    let c_name = CString::new(name).expect("toCString refused a NUL");
    let aggregate = matches!(kind, Kind::Aggregate { .. });
    let context = heap::into_raw(Box::new(Callbacks {
        connection: Rc::downgrade(connection),
        name: name.to_owned(),
        safe_integers,
        kind,
    })) as usize;
    let mut ok = false;
    conn.register_static_extension(|api| {
        // SAFETY: `api.ctx` is the connection's for the extent of this closure; `context` is a leaked `Callbacks`
        // that the connection owns from here on and frees with `destroy`.
        let code = unsafe {
            if aggregate {
                (api.register_aggregate_function)(
                    api.ctx,
                    c_name.as_ptr(),
                    argument_count,
                    context,
                    init_aggregate,
                    step_aggregate,
                    finalize_aggregate,
                    Some(destroy),
                    Some(destroy_aggregate),
                    None,
                )
            } else {
                (api.register_scalar_function)(
                    api.ctx,
                    c_name.as_ptr(),
                    argument_count,
                    deterministic,
                    context,
                    call_scalar,
                    Some(destroy),
                    None,
                )
            }
        };
        ok = code.is_ok();
    });
    if !ok {
        // SAFETY: refused, so the connection did not take it.
        unsafe { destroy(context) };
        return Err(SqliteError::new(SQLITE_MISUSE, errstr(SQLITE_MISUSE)));
    }
    Ok(())
}
