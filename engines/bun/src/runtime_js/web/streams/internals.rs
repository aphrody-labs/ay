//! What every stream class uses: WebStreamsInternals.h, StreamQueue.h and the strategy parts of WebStreamsMisc.cpp.
//! Promises are made, settled and reacted to here, so that a reaction is a Rust function with the values it
//! needs, traced by Boa's collector, as the C++ keeps them in an algorithm context cell.

use std::collections::VecDeque;

use boa_engine::object::FunctionObjectBuilder;
use boa_engine::object::builtins::JsPromise;
use boa_engine::{JsArgs, NativeFunction};
use boa_gc::{Finalize, Trace};
use bun_js::{ErrorCode, JSGlobalObject, JSObject, JSPromise, JSValue, JsError, JsResult};

use crate::{cx, lift};

/// A promise and the functions that settle it.
#[derive(Clone, Trace, Finalize)]
#[boa_gc(unsafe_no_drop)]
pub struct Deferred {
    pub promise: JSValue,
    resolve: JSValue,
    reject: JSValue,
}

impl Deferred {
    pub fn new(global: &JSGlobalObject) -> Deferred {
        let (promise, functions) = JsPromise::new_pending(cx(global));
        Deferred {
            promise: JSValue::from_engine(promise.into()),
            resolve: JSValue::from_engine(functions.resolve.into()),
            reject: JSValue::from_engine(functions.reject.into()),
        }
    }

    /// A promise already rejected with `reason`, which nothing reports as unhandled.
    pub fn rejected_handled(global: &JSGlobalObject, reason: JSValue) -> Deferred {
        let deferred = Deferred::new(global);
        deferred.reject(global, reason);
        mark_handled(global, &deferred.promise);
        deferred
    }

    pub fn resolve(&self, global: &JSGlobalObject, value: JSValue) {
        settle(global, &self.resolve, value);
    }

    pub fn reject(&self, global: &JSGlobalObject, reason: JSValue) {
        settle(global, &self.reject, reason);
    }
}

fn settle(global: &JSGlobalObject, function: &JSValue, value: JSValue) {
    // A resolving function throws nothing of its own; what a thenable's `then` getter throws rejects the promise.
    if function.call(global, JSValue::UNDEFINED, &[value]).is_err() {
        global.clear_exception();
    }
}

/// `PromiseResolve(%Promise%, value)`.
pub fn resolved(global: &JSGlobalObject, value: JSValue) -> JSValue {
    JSPromise::resolved_promise_value(global, value)
}

/// A promise rejected with `reason`.
pub fn rejected(global: &JSGlobalObject, reason: JSValue) -> JSValue {
    JSPromise::rejected_promise_value(global, reason)
}

/// What a reaction does with the value or the reason, and the values it needs.
pub type Reaction<C> = fn(&JSGlobalObject, &C, JSValue) -> JsResult<JSValue>;

fn reaction_function<C: Trace + Finalize + Clone + 'static>(
    global: &JSGlobalObject,
    captures: &C,
    reaction: Reaction<C>,
) -> boa_engine::object::builtins::JsFunction {
    let realm = cx(global).realm().clone();
    let function = NativeFunction::from_copy_closure_with_captures(
        move |_, args, captures: &C, context| {
            let global = JSGlobalObject::from_context(context);
            let value = JSValue::from_engine(args.get_or_undefined(0).clone());
            lift(global, reaction(global, captures, value)).map(JSValue::into_engine)
        },
        captures.clone(),
    );
    FunctionObjectBuilder::new(&realm, function).length(1).build()
}

fn as_promise(global: &JSGlobalObject, promise: &JSValue) -> Option<JsPromise> {
    let object = promise.as_engine().as_object();
    match object.and_then(|object| JsPromise::from_object(object.clone()).ok()) {
        Some(promise) => Some(promise),
        None => JsPromise::resolve(promise.as_engine().clone(), cx(global)).ok(),
    }
}

/// "Reacting to a promise": the promise of what the reaction returns, rejected with what it throws. A missing
/// reaction passes the value or the reason on.
pub fn react<C: Trace + Finalize + Clone + 'static>(
    global: &JSGlobalObject,
    promise: &JSValue,
    captures: C,
    on_fulfilled: Option<Reaction<C>>,
    on_rejected: Option<Reaction<C>>,
) -> JSValue {
    let Some(promise) = as_promise(global, promise) else {
        return JSValue::UNDEFINED;
    };
    let on_fulfilled = on_fulfilled.map(|reaction| reaction_function(global, &captures, reaction));
    let on_rejected = on_rejected.map(|reaction| reaction_function(global, &captures, reaction));
    match promise.then(on_fulfilled, on_rejected, cx(global)) {
        Ok(derived) => JSValue::from_engine(derived.into()),
        Err(_) => JSValue::UNDEFINED,
    }
}

/// The captures of a reaction that needs none.
#[derive(Clone, Trace, Finalize)]
#[boa_gc(unsafe_no_drop)]
pub struct Nothing;

fn ignore<C>(_: &JSGlobalObject, _: &C, _: JSValue) -> JsResult<JSValue> {
    Ok(JSValue::UNDEFINED)
}

/// "Upon fulfillment / upon rejection": steps that run when the promise settles, whose own result nothing sees.
pub fn upon<C: Trace + Finalize + Clone + 'static>(
    global: &JSGlobalObject,
    promise: &JSValue,
    captures: C,
    on_fulfilled: Option<Reaction<C>>,
    on_rejected: Option<Reaction<C>>,
) {
    let derived = react(
        global,
        promise,
        captures,
        Some(on_fulfilled.unwrap_or(ignore::<C>)),
        Some(on_rejected.unwrap_or(ignore::<C>)),
    );
    mark_handled(global, &derived);
}

/// `[[PromiseIsHandled]] = true`: a rejection of `promise` is not reported.
pub fn mark_handled(global: &JSGlobalObject, promise: &JSValue) {
    if let Some(promise) = promise.as_engine().as_object().and_then(|o| JsPromise::from_object(o.clone()).ok()) {
        let on_rejected = reaction_function(global, &Nothing, ignore::<Nothing>);
        let _ = promise.then(None, Some(on_rejected), cx(global));
    }
}

/// The exception of a call that threw, where script can catch it; termination goes on unwinding.
pub fn catch(global: &JSGlobalObject, error: JsError) -> JsResult<JSValue> {
    match error {
        JsError::Thrown => match global.peek_exception() {
            Some(exception) if global.clear_catchable_exception() => Ok(exception),
            _ => Err(JsError::Thrown),
        },
        other => Err(other),
    }
}

/// The promise of what `body` returns, or rejected with what it throws.
pub fn promise_of(global: &JSGlobalObject, body: impl FnOnce() -> JsResult<JSValue>) -> JsResult<JSValue> {
    match body() {
        Ok(value) => Ok(resolved(global, value)),
        Err(error) => Ok(rejected(global, catch(global, error)?)),
    }
}

/// "Invoke `method` on `this` with `args`, as a promise": `undefined` for a method that is not there.
pub fn promise_call(
    global: &JSGlobalObject,
    method: &JSValue,
    this: &JSValue,
    args: &[JSValue],
) -> JsResult<JSValue> {
    if !method.is_callable() {
        return Ok(resolved(global, JSValue::UNDEFINED));
    }
    promise_of(global, || method.call(global, this.clone(), args))
}

/// `CreateIterResultObject(value, done)`.
pub fn iter_result(global: &JSGlobalObject, value: JSValue, done: bool) -> JSValue {
    let object = JSObject::create(global);
    object.put(global, "value", value);
    object.put(global, "done", JSValue::js_boolean(done));
    object.to_js()
}

/// An `ERR_INVALID_STATE` TypeError: `Invalid state: <what>`.
pub fn invalid_state(global: &JSGlobalObject, what: &str) -> JSValue {
    ErrorCode::INVALID_STATE_TypeError.fmt(global, format_args!("Invalid state: {what}"))
}

pub fn type_error(global: &JSGlobalObject, message: &str) -> JSValue {
    global.create_type_error_instance(format_args!("{message}"))
}

/// A member of a dictionary argument: `undefined` where the argument is `undefined` or `null`.
pub fn member(global: &JSGlobalObject, dictionary: &JSValue, name: &str) -> JsResult<JSValue> {
    if dictionary.is_undefined_or_null() {
        return Ok(JSValue::UNDEFINED);
    }
    Ok(dictionary.get(global, name)?.unwrap_or_default())
}

/// A callback member: `undefined`, or a function; anything else throws `<what>'s '<name>' property must be a function`.
pub fn callback_member(global: &JSGlobalObject, dictionary: &JSValue, name: &str, what: &str) -> JsResult<JSValue> {
    let value = member(global, dictionary, name)?;
    if !value.is_undefined() && !value.is_callable() {
        return Err(global.throw_type_error(format_args!("{what}'s '{name}' property must be a function")));
    }
    Ok(value)
}

/// A converted `QueuingStrategy`: its `highWaterMark` (unchecked) and `size`.
pub struct Strategy {
    pub high_water_mark: Option<f64>,
    pub size: JSValue,
}

/// The `QueuingStrategy` dictionary: `undefined` and `null` are `{}`, anything else that is not an object throws.
pub fn strategy(global: &JSGlobalObject, value: &JSValue, not_object: &str) -> JsResult<Strategy> {
    if !value.is_undefined_or_null() && !value.is_object() {
        return Err(global.throw_type_error(format_args!("{not_object}")));
    }
    let high_water_mark = match member(global, value, "highWaterMark")? {
        hwm if hwm.is_undefined() => None,
        hwm => Some(hwm.to_number(global)?),
    };
    let size = member(global, value, "size")?;
    if !size.is_undefined() && !size.is_callable() {
        return Err(global.throw_type_error(format_args!("The queuing strategy's 'size' property must be a function")));
    }
    Ok(Strategy { high_water_mark, size })
}

/// `ExtractHighWaterMark(strategy, defaultHWM)`.
pub fn extract_high_water_mark(global: &JSGlobalObject, strategy: &Strategy, default: f64) -> JsResult<f64> {
    let Some(high_water_mark) = strategy.high_water_mark else {
        return Ok(default);
    };
    if high_water_mark.is_nan() || high_water_mark < 0.0 {
        return Err(global.throw_range_error(format_args!(
            "The queuing strategy's highWaterMark must be a non-negative, non-NaN number"
        )));
    }
    Ok(high_water_mark)
}

/// A strategy's `size(chunk)`, which is 1 without one.
pub fn chunk_size(global: &JSGlobalObject, size: &JSValue, chunk: &JSValue) -> JsResult<f64> {
    if size.is_undefined() {
        return Ok(1.0);
    }
    size.call(global, JSValue::UNDEFINED, std::slice::from_ref(chunk))?
        .to_number(global)
}

#[derive(Clone, Trace, Finalize)]
#[boa_gc(unsafe_no_drop)]
struct QueueEntry {
    value: JSValue,
    size: f64,
}

/// A queue with sizes (`[[queue]]`, `[[queueTotalSize]]`).
#[derive(Default, Trace, Finalize)]
pub struct Queue {
    entries: VecDeque<QueueEntry>,
    total_size: f64,
}

impl Queue {
    /// `EnqueueValueWithSize`.
    pub fn enqueue(&mut self, global: &JSGlobalObject, value: JSValue, size: f64) -> JsResult<()> {
        if !size.is_finite() || size < 0.0 {
            return Err(global.throw_range_error(format_args!(
                "The queuing strategy's chunk size must be a non-negative, finite number"
            )));
        }
        self.entries.push_back(QueueEntry { value, size });
        self.total_size += size;
        Ok(())
    }

    /// `DequeueValue`.
    pub fn dequeue(&mut self) -> JSValue {
        let Some(entry) = self.entries.pop_front() else {
            return JSValue::UNDEFINED;
        };
        self.total_size -= entry.size;
        if self.total_size < 0.0 {
            self.total_size = 0.0;
        }
        entry.value
    }

    /// `PeekQueueValue`.
    pub fn peek(&self) -> JSValue {
        self.entries.front().map(|entry| entry.value.clone()).unwrap_or_default()
    }

    /// `ResetQueue`.
    pub fn reset(&mut self) {
        self.entries.clear();
        self.total_size = 0.0;
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn total_size(&self) -> f64 {
        self.total_size
    }
}
