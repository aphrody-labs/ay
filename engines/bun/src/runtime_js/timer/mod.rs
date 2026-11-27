//! `setTimeout`, `setInterval`, `setImmediate` and their `clear*`, as Bun's (src/runtime/timer): they return a
//! `Timeout` or an `Immediate` with `ref()`, `unref()`, `hasRef()`, and for a `Timeout` `refresh()`, `close()` and
//! its id as `[Symbol.toPrimitive]()`. A delay that is not a number from 1 to 2^31-1 is 1 ms.

use std::time::Duration;

use boa_engine::object::FunctionObjectBuilder;
use boa_engine::property::PropertyDescriptor;
use boa_engine::{Context, JsObject, JsResult, JsString, JsSymbol, JsValue, NativeFunction};
use boa_gc::{Finalize, Trace};
use bun_js::class::{Attributes, ClassBuilder};
use bun_js::{CallFrame, ErrorCode, JSGlobalObject, JSValue, JsClass};

use crate::event_loop::{Callback, EventLoop};
use crate::{cx, lift};

#[derive(Trace, Finalize)]
pub struct Timeout {
    #[unsafe_ignore_trace]
    id: u32,
    #[unsafe_ignore_trace]
    repeat: bool,
}

#[derive(Trace, Finalize)]
pub struct Immediate {
    #[unsafe_ignore_trace]
    id: u32,
}

const HIDDEN: Attributes = Attributes {
    writable: true,
    enumerable: false,
    configurable: true,
};

fn this_timeout(this: &bun_js::ClassRef<Timeout>, global: &JSGlobalObject) -> bun_js::JsResult<u32> {
    Ok(this.borrow(global)?.id)
}

impl JsClass for Timeout {
    const NAME: &'static str = "Timeout";

    fn define(class: &mut ClassBuilder<'_, Self>) -> bun_js::JsResult<()> {
        class.method("ref", 0, HIDDEN, |this, global, _| {
            EventLoop::of(cx(global)).set_timer_ref(this_timeout(this, global)?, true);
            Ok(this.to_js())
        })?;
        class.method("unref", 0, HIDDEN, |this, global, _| {
            EventLoop::of(cx(global)).set_timer_ref(this_timeout(this, global)?, false);
            Ok(this.to_js())
        })?;
        class.method("hasRef", 0, HIDDEN, |this, global, _| {
            let id = this_timeout(this, global)?;
            Ok(JSValue::js_boolean(EventLoop::of(cx(global)).timer_has_ref(id)))
        })?;
        class.method("refresh", 0, HIDDEN, |this, global, _| {
            EventLoop::of(cx(global)).refresh_timer(this_timeout(this, global)?);
            Ok(this.to_js())
        })?;
        class.method("close", 0, HIDDEN, |this, global, _| {
            EventLoop::of(cx(global)).clear_timer(this_timeout(this, global)?);
            Ok(this.to_js())
        })?;
        class.getter("_destroyed", HIDDEN, |this, global| {
            let id = this_timeout(this, global)?;
            Ok(JSValue::js_boolean(!EventLoop::of(cx(global)).has_timer(id)))
        })?;
        class.getter("_repeat", HIDDEN, |this, global| {
            Ok(JSValue::js_boolean(this.borrow(global)?.repeat))
        })?;
        Ok(())
    }
}

impl JsClass for Immediate {
    const NAME: &'static str = "Immediate";

    fn define(class: &mut ClassBuilder<'_, Self>) -> bun_js::JsResult<()> {
        class.method("ref", 0, HIDDEN, |this, global, _| {
            let id = this.borrow(global)?.id;
            EventLoop::of(cx(global)).set_immediate_ref(id, true);
            Ok(this.to_js())
        })?;
        class.method("unref", 0, HIDDEN, |this, global, _| {
            let id = this.borrow(global)?.id;
            EventLoop::of(cx(global)).set_immediate_ref(id, false);
            Ok(this.to_js())
        })?;
        class.method("hasRef", 0, HIDDEN, |this, global, _| {
            let id = this.borrow(global)?.id;
            Ok(JSValue::js_boolean(EventLoop::of(cx(global)).immediate_has_ref(id)))
        })?;
        Ok(())
    }
}

fn callback_argument(global: &JSGlobalObject, frame: &CallFrame<'_>) -> bun_js::JsResult<JsObject> {
    let callback = frame.argument(0);
    match callback.as_engine().as_callable() {
        Some(function) => Ok(function.clone()),
        None => {
            let received = bun_js::bindgen::determine_specific_type(global, &callback)?;
            Err(ErrorCode::INVALID_ARG_TYPE.throw(
                global,
                format_args!("The \"callback\" argument must be of type function. Received {received}"),
            ))
        }
    }
}

/// Bun's `jsValueToCountdown` for `setTimeout`: whole milliseconds, 1 if out of `1..=2^31-1`.
fn delay(global: &JSGlobalObject, value: &JSValue) -> bun_js::JsResult<Duration> {
    let ms = if value.is_undefined() { 0.0 } else { value.to_number(global)? };
    let ms = if (1.0..=f64::from(i32::MAX)).contains(&ms) { ms as u64 } else { 1 };
    Ok(Duration::from_millis(ms))
}

fn rest(frame: &CallFrame<'_>, from: usize) -> Vec<JsValue> {
    frame.arguments().iter().skip(from).map(|v| v.as_engine().clone()).collect()
}

fn set_timer(global: &JSGlobalObject, frame: &CallFrame<'_>, repeat: bool) -> bun_js::JsResult<JSValue> {
    let function = callback_argument(global, frame)?;
    let delay = delay(global, &frame.argument(1))?;
    let callback = Callback::Function {
        function,
        args: rest(frame, 2),
    };
    let id = EventLoop::of(cx(global)).add_timer(callback, delay, repeat);
    bun_js::class::to_js(global, Timeout { id, repeat })
}

fn set_timeout(global: &JSGlobalObject, frame: &CallFrame<'_>) -> bun_js::JsResult<JSValue> {
    set_timer(global, frame, false)
}

fn set_interval(global: &JSGlobalObject, frame: &CallFrame<'_>) -> bun_js::JsResult<JSValue> {
    set_timer(global, frame, true)
}

fn set_immediate(global: &JSGlobalObject, frame: &CallFrame<'_>) -> bun_js::JsResult<JSValue> {
    let function = callback_argument(global, frame)?;
    let id = EventLoop::of(cx(global)).add_immediate(function, rest(frame, 1));
    bun_js::class::to_js(global, Immediate { id })
}

/// The id a `clear*` argument names: a `Timeout`, an `Immediate`, a number or a numeric string.
fn id_of(global: &JSGlobalObject, value: &JSValue) -> bun_js::JsResult<Option<u32>> {
    if let Some(timeout) = value.as_class_ref::<Timeout>() {
        return Ok(Some(timeout.borrow(global)?.id));
    }
    if let Some(immediate) = value.as_class_ref::<Immediate>() {
        return Ok(Some(immediate.borrow(global)?.id));
    }
    if value.is_number() || value.is_string() {
        let n = value.to_number(global)?;
        if n.is_finite() && n >= 1.0 && n.fract() == 0.0 && n <= f64::from(u32::MAX) {
            return Ok(Some(n as u32));
        }
    }
    Ok(None)
}

fn clear_timeout(global: &JSGlobalObject, frame: &CallFrame<'_>) -> bun_js::JsResult<JSValue> {
    if let Some(id) = id_of(global, &frame.argument(0))? {
        EventLoop::of(cx(global)).clear_timer(id);
    }
    Ok(JSValue::UNDEFINED)
}

fn clear_immediate(global: &JSGlobalObject, frame: &CallFrame<'_>) -> bun_js::JsResult<JSValue> {
    if let Some(id) = id_of(global, &frame.argument(0))? {
        EventLoop::of(cx(global)).clear_immediate(id);
    }
    Ok(JSValue::UNDEFINED)
}

fn timeout_to_primitive(this: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let global = JSGlobalObject::from_context(context);
    let this = JSValue::from_engine(this.clone());
    Ok(match this.as_class_ref::<Timeout>() {
        Some(timeout) => lift(global, timeout.borrow(global).map(|t| t.id))?.into(),
        None => JsValue::nan(),
    })
}

/// Defines the timer functions on the global object.
pub fn install(context: &mut Context) -> JsResult<()> {
    let global = JSGlobalObject::from_context(context);
    let functions: [(&str, bun_js::JSHostFn, usize); 6] = [
        ("setTimeout", set_timeout, 1),
        ("setInterval", set_interval, 1),
        ("setImmediate", set_immediate, 1),
        ("clearTimeout", clear_timeout, 1),
        ("clearInterval", clear_timeout, 1),
        ("clearImmediate", clear_immediate, 1),
    ];
    let functions: Vec<(&str, JsValue)> = functions
        .into_iter()
        .map(|(name, f, length)| (name, bun_js::JSFunction::create(global, name, f, length).into_engine()))
        .collect();
    for (name, function) in functions {
        crate::define_global(context, name, function)?;
    }
    let global = JSGlobalObject::from_context(context);
    let constructor = lift(global, bun_js::class::constructor::<Timeout>(global))?;
    let prototype = constructor
        .as_engine()
        .as_object()
        .map(|c| c.get(JsString::from("prototype"), context))
        .transpose()?
        .and_then(|p| p.as_object());
    if let Some(prototype) = prototype {
        let to_primitive = FunctionObjectBuilder::new(context.realm(), NativeFunction::from_fn_ptr(timeout_to_primitive))
            .name(JsString::from("[Symbol.toPrimitive]"))
            .length(1)
            .build();
        prototype.define_property_or_throw(
            JsSymbol::to_primitive(),
            PropertyDescriptor::builder()
                .value(to_primitive)
                .writable(true)
                .enumerable(false)
                .configurable(true),
            context,
        )?;
    }
    let global = JSGlobalObject::from_context(context);
    lift(global, bun_js::class::constructor::<Immediate>(global))?;
    Ok(())
}
