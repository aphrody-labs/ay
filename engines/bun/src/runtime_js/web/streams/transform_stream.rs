//! `TransformStream` (Streams §6.2, §6.4.1, §6.4.3, §6.4.4): JSTransformStream.cpp and TransformStreamOperations.cpp.

use boa_gc::{Finalize, Trace};
use bun_js::class::{Attributes, ClassBuilder, ClassRef};
use bun_js::{CallFrame, JSGlobalObject, JSValue, JsClass, JsResult};

use super::internals::{self, Deferred, Reaction, callback_member, member};
use super::readable_stream::{self, State as ReadableState};
use super::readable_stream_default_controller::{self as readable_controller, SourceAlgorithms};
use super::transform_stream_default_controller::{self as controller, TransformAlgorithms};
use super::writable_stream::{self, State as WritableState};
use super::writable_stream_default_controller::{self as writable_controller, SinkAlgorithms};

#[derive(Trace, Finalize)]
pub struct TransformStream {
    pub(crate) readable: JSValue,
    pub(crate) writable: JSValue,
    pub(crate) backpressure: Option<bool>,
    pub(crate) backpressure_change: Option<Deferred>,
    pub(crate) controller: JSValue,
}

pub fn stream_of(value: &JSValue) -> ClassRef<TransformStream> {
    value.as_class_ref::<TransformStream>().expect("a TransformStream")
}

fn fields(global: &JSGlobalObject, stream: &JSValue) -> JsResult<(JSValue, JSValue, JSValue)> {
    let s = stream_of(stream);
    let s = s.borrow(global)?;
    Ok((s.readable.clone(), s.writable.clone(), s.controller.clone()))
}

fn readable_controller_of(global: &JSGlobalObject, readable: &JSValue) -> JsResult<JSValue> {
    Ok(readable_stream::stream_of(readable).borrow(global)?.controller.clone())
}

fn writable_controller_of(global: &JSGlobalObject, writable: &JSValue) -> JsResult<JSValue> {
    Ok(writable_stream::stream_of(writable).borrow(global)?.controller.clone())
}

/// `InitializeTransformStream`.
fn initialize_transform_stream(
    global: &JSGlobalObject,
    stream: &JSValue,
    start_promise: &JSValue,
    writable_high_water_mark: f64,
    writable_size: JSValue,
    readable_high_water_mark: f64,
    readable_size: JSValue,
) -> JsResult<()> {
    let writable = writable_stream::create(
        global,
        start_promise.clone(),
        SinkAlgorithms::Transform { stream: stream.clone() },
        writable_high_water_mark,
        writable_size,
    )?;
    let readable = readable_stream::create_with(
        global,
        start_promise.clone(),
        SourceAlgorithms::Transform { stream: stream.clone() },
        readable_high_water_mark,
        readable_size,
    )?;
    {
        let s = stream_of(stream);
        let mut s = s.borrow_mut(global)?;
        s.writable = writable;
        s.readable = readable;
        s.backpressure = None;
        s.backpressure_change = None;
    }
    set_backpressure(global, stream, true)
}

/// `TransformStreamError`.
pub fn error(global: &JSGlobalObject, stream: &JSValue, reason: JSValue) -> JsResult<()> {
    let (readable, _, _) = fields(global, stream)?;
    readable_controller::error(global, &readable_controller_of(global, &readable)?, reason.clone())?;
    error_writable_and_unblock_write(global, stream, reason)
}

/// `TransformStreamErrorWritableAndUnblockWrite`.
pub fn error_writable_and_unblock_write(global: &JSGlobalObject, stream: &JSValue, reason: JSValue) -> JsResult<()> {
    let (_, writable, controller) = fields(global, stream)?;
    controller::clear_algorithms(global, &controller)?;
    writable_controller::error_if_needed(global, &writable_controller_of(global, &writable)?, reason)?;
    unblock_write(global, stream)
}

/// `TransformStreamSetBackpressure`.
pub fn set_backpressure(global: &JSGlobalObject, stream: &JSValue, backpressure: bool) -> JsResult<()> {
    let previous = {
        let s = stream_of(stream);
        let mut s = s.borrow_mut(global)?;
        let previous = s.backpressure_change.replace(Deferred::new(global));
        s.backpressure = Some(backpressure);
        previous
    };
    if let Some(previous) = previous {
        previous.resolve(global, JSValue::UNDEFINED);
    }
    Ok(())
}

/// `TransformStreamUnblockWrite`.
pub fn unblock_write(global: &JSGlobalObject, stream: &JSValue) -> JsResult<()> {
    if stream_of(stream).borrow(global)?.backpressure == Some(true) {
        set_backpressure(global, stream, false)?;
    }
    Ok(())
}

pub fn backpressure(global: &JSGlobalObject, stream: &JSValue) -> JsResult<Option<bool>> {
    Ok(stream_of(stream).borrow(global)?.backpressure)
}

/// `TransformStreamDefaultSinkWriteAlgorithm`.
pub fn sink_write(global: &JSGlobalObject, stream: &JSValue, chunk: JSValue) -> JsResult<JSValue> {
    let (backpressure, change, controller) = {
        let s = stream_of(stream);
        let s = s.borrow(global)?;
        (s.backpressure, s.backpressure_change.clone(), s.controller.clone())
    };
    if backpressure == Some(true)
        && let Some(change) = change
    {
        #[derive(Clone, Trace, Finalize)]
        #[boa_gc(unsafe_no_drop)]
        struct Write {
            stream: JSValue,
            controller: JSValue,
            chunk: JSValue,
        }
        let after: Reaction<Write> = |global, write, _| {
            let (_, writable, _) = fields(global, &write.stream)?;
            if writable_stream::state(global, &writable)? == WritableState::Erroring {
                return Err(global.throw_value(writable_stream::stored_error(global, &writable)?));
            }
            controller::perform_transform(global, &write.controller, write.chunk.clone())
        };
        let write = Write {
            stream: stream.clone(),
            controller,
            chunk,
        };
        return Ok(internals::react(global, &change.promise, write, Some(after), None));
    }
    controller::perform_transform(global, &controller, chunk)
}

#[derive(Clone, Trace, Finalize)]
#[boa_gc(unsafe_no_drop)]
struct Finish {
    stream: JSValue,
    controller: JSValue,
    reason: JSValue,
}

fn finish_promise(global: &JSGlobalObject, controller: &JSValue) -> JsResult<Option<JSValue>> {
    Ok(controller::controller_of(controller)
        .borrow(global)?
        .finish
        .as_ref()
        .map(|finish| finish.promise.clone()))
}

fn start_finish(global: &JSGlobalObject, controller: &JSValue) -> JsResult<()> {
    controller::controller_of(controller).borrow_mut(global)?.finish = Some(Deferred::new(global));
    Ok(())
}

fn finish_deferred(global: &JSGlobalObject, controller: &JSValue) -> JsResult<Deferred> {
    Ok(controller::controller_of(controller)
        .borrow(global)?
        .finish
        .clone()
        .expect("a finish promise"))
}

/// `TransformStreamDefaultSinkAbortAlgorithm`.
pub fn sink_abort(global: &JSGlobalObject, stream: &JSValue, reason: JSValue) -> JsResult<JSValue> {
    let (_, _, controller) = fields(global, stream)?;
    if let Some(finish) = finish_promise(global, &controller)? {
        return Ok(finish);
    }
    start_finish(global, &controller)?;
    let cancel = controller::cancel_algorithm(global, &controller, reason.clone())?;
    controller::clear_algorithms(global, &controller)?;
    let fulfilled: Reaction<Finish> = |global, f, _| {
        let (readable, _, _) = fields(global, &f.stream)?;
        let finish = finish_deferred(global, &f.controller)?;
        if readable_stream::state(global, &readable)? == ReadableState::Errored {
            finish.reject(global, readable_stream::stream_of(&readable).borrow(global)?.stored_error.clone());
        } else {
            readable_controller::error(global, &readable_controller_of(global, &readable)?, f.reason.clone())?;
            finish.resolve(global, JSValue::UNDEFINED);
        }
        Ok(JSValue::UNDEFINED)
    };
    let rejected: Reaction<Finish> = |global, f, reason| {
        let (readable, _, _) = fields(global, &f.stream)?;
        readable_controller::error(global, &readable_controller_of(global, &readable)?, reason.clone())?;
        finish_deferred(global, &f.controller)?.reject(global, reason);
        Ok(JSValue::UNDEFINED)
    };
    let f = Finish {
        stream: stream.clone(),
        controller: controller.clone(),
        reason,
    };
    internals::upon(global, &cancel, f, Some(fulfilled), Some(rejected));
    Ok(finish_deferred(global, &controller)?.promise)
}

/// `TransformStreamDefaultSinkCloseAlgorithm`.
pub fn sink_close(global: &JSGlobalObject, stream: &JSValue) -> JsResult<JSValue> {
    let (_, _, controller) = fields(global, stream)?;
    if let Some(finish) = finish_promise(global, &controller)? {
        return Ok(finish);
    }
    start_finish(global, &controller)?;
    let flush = controller::flush_algorithm(global, &controller)?;
    controller::clear_algorithms(global, &controller)?;
    let fulfilled: Reaction<Finish> = |global, f, _| {
        let (readable, _, _) = fields(global, &f.stream)?;
        let finish = finish_deferred(global, &f.controller)?;
        if readable_stream::state(global, &readable)? == ReadableState::Errored {
            finish.reject(global, readable_stream::stream_of(&readable).borrow(global)?.stored_error.clone());
        } else {
            readable_controller::close(global, &readable_controller_of(global, &readable)?)?;
            finish.resolve(global, JSValue::UNDEFINED);
        }
        Ok(JSValue::UNDEFINED)
    };
    let rejected: Reaction<Finish> = |global, f, reason| {
        let (readable, _, _) = fields(global, &f.stream)?;
        readable_controller::error(global, &readable_controller_of(global, &readable)?, reason.clone())?;
        finish_deferred(global, &f.controller)?.reject(global, reason);
        Ok(JSValue::UNDEFINED)
    };
    let f = Finish {
        stream: stream.clone(),
        controller: controller.clone(),
        reason: JSValue::UNDEFINED,
    };
    internals::upon(global, &flush, f, Some(fulfilled), Some(rejected));
    Ok(finish_deferred(global, &controller)?.promise)
}

/// `TransformStreamDefaultSourcePullAlgorithm`.
pub fn source_pull(global: &JSGlobalObject, stream: &JSValue) -> JsResult<JSValue> {
    set_backpressure(global, stream, false)?;
    let change = stream_of(stream).borrow(global)?.backpressure_change.clone();
    Ok(change.map(|change| change.promise).unwrap_or_default())
}

/// `TransformStreamDefaultSourceCancelAlgorithm`.
pub fn source_cancel(global: &JSGlobalObject, stream: &JSValue, reason: JSValue) -> JsResult<JSValue> {
    let (_, _, controller) = fields(global, stream)?;
    if let Some(finish) = finish_promise(global, &controller)? {
        return Ok(finish);
    }
    start_finish(global, &controller)?;
    let cancel = controller::cancel_algorithm(global, &controller, reason.clone())?;
    controller::clear_algorithms(global, &controller)?;
    let fulfilled: Reaction<Finish> = |global, f, _| {
        let (_, writable, _) = fields(global, &f.stream)?;
        let finish = finish_deferred(global, &f.controller)?;
        if writable_stream::state(global, &writable)? == WritableState::Errored {
            finish.reject(global, writable_stream::stored_error(global, &writable)?);
        } else {
            writable_controller::error_if_needed(global, &writable_controller_of(global, &writable)?, f.reason.clone())?;
            unblock_write(global, &f.stream)?;
            finish.resolve(global, JSValue::UNDEFINED);
        }
        Ok(JSValue::UNDEFINED)
    };
    let rejected: Reaction<Finish> = |global, f, reason| {
        let (_, writable, _) = fields(global, &f.stream)?;
        writable_controller::error_if_needed(global, &writable_controller_of(global, &writable)?, reason.clone())?;
        unblock_write(global, &f.stream)?;
        finish_deferred(global, &f.controller)?.reject(global, reason);
        Ok(JSValue::UNDEFINED)
    };
    let f = Finish {
        stream: stream.clone(),
        controller: controller.clone(),
        reason,
    };
    internals::upon(global, &cancel, f, Some(fulfilled), Some(rejected));
    Ok(finish_deferred(global, &controller)?.promise)
}

/// A TransformStream whose transformer is Rust, with the default strategies.
pub fn create_native(global: &JSGlobalObject, state: super::native_transform::NativeTransform) -> JsResult<JSValue> {
    let stream = bun_js::class::to_js(
        global,
        TransformStream {
            readable: JSValue::UNDEFINED,
            writable: JSValue::UNDEFINED,
            backpressure: None,
            backpressure_change: None,
            controller: JSValue::UNDEFINED,
        },
    )?;
    let start = internals::resolved(global, JSValue::UNDEFINED);
    initialize_transform_stream(global, &stream, &start, 1.0, JSValue::UNDEFINED, 0.0, JSValue::UNDEFINED)?;
    controller::set_up(global, &stream, TransformAlgorithms::Native(state))?;
    Ok(stream)
}

/// The TransformStream constructor's steps, on the new stream.
fn construct_steps(global: &JSGlobalObject, stream: &JSValue, frame: &CallFrame<'_>) -> JsResult<()> {
    let [transformer, writable_strategy, readable_strategy] = frame.arguments_as_array::<3>();
    if !transformer.is_undefined_or_null() && !transformer.is_object() {
        return Err(global.throw_type_error(format_args!(
            "TransformStream constructor takes an object as first argument"
        )));
    }
    // The Transformer dictionary, its members in order.
    let what = "The transformer";
    let cancel = callback_member(global, &transformer, "cancel", what)?;
    let flush = callback_member(global, &transformer, "flush", what)?;
    let readable_type = member(global, &transformer, "readableType")?;
    let start = callback_member(global, &transformer, "start", what)?;
    let transform = callback_member(global, &transformer, "transform", what)?;
    let writable_type = member(global, &transformer, "writableType")?;
    let writable_strategy = internals::strategy(global, &writable_strategy, "The queuing strategy must be an object")?;
    let readable_strategy = internals::strategy(global, &readable_strategy, "The queuing strategy must be an object")?;
    if !readable_type.is_undefined() {
        return Err(global.throw_range_error(format_args!(
            "The transformer's 'readableType' property is reserved and must not be present"
        )));
    }
    if !writable_type.is_undefined() {
        return Err(global.throw_range_error(format_args!(
            "The transformer's 'writableType' property is reserved and must not be present"
        )));
    }
    let readable_high_water_mark = internals::extract_high_water_mark(global, &readable_strategy, 0.0)?;
    let writable_high_water_mark = internals::extract_high_water_mark(global, &writable_strategy, 1.0)?;
    let start_promise = Deferred::new(global);
    initialize_transform_stream(
        global,
        stream,
        &start_promise.promise,
        writable_high_water_mark,
        writable_strategy.size,
        readable_high_water_mark,
        readable_strategy.size,
    )?;
    let algorithms = TransformAlgorithms::Js {
        transformer: transformer.clone(),
        transform,
        flush,
        cancel,
    };
    let controller = controller::set_up(global, stream, algorithms)?;
    if start.is_callable() {
        let started = start.call(global, transformer, &[controller])?;
        start_promise.resolve(global, started);
    } else {
        start_promise.resolve(global, JSValue::UNDEFINED);
    }
    Ok(())
}

impl JsClass for TransformStream {
    const NAME: &'static str = "TransformStream";

    fn construct(_: &JSGlobalObject, _: &CallFrame<'_>) -> JsResult<Self> {
        Ok(TransformStream {
            readable: JSValue::UNDEFINED,
            writable: JSValue::UNDEFINED,
            backpressure: None,
            backpressure_change: None,
            controller: JSValue::UNDEFINED,
        })
    }

    fn initialize(this: &ClassRef<Self>, global: &JSGlobalObject, frame: &CallFrame<'_>) -> JsResult<()> {
        construct_steps(global, &this.to_js(), frame)
    }

    fn define(class: &mut ClassBuilder<'_, Self>) -> JsResult<()> {
        let attributes = Attributes::DEFAULT;
        class.getter("readable", attributes, |this, global| Ok(this.borrow(global)?.readable.clone()))?;
        class.getter("writable", attributes, |this, global| Ok(this.borrow(global)?.writable.clone()))?;
        Ok(())
    }
}
