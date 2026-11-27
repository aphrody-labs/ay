//! `WritableStreamDefaultController` (Streams §5.4, §5.5.4): JSWritableStreamDefaultController.cpp.

use boa_gc::{Finalize, Trace};
use bun_js::class::{Attributes, ClassBuilder, ClassRef};
use bun_js::{JSGlobalObject, JSObject, JSValue, JsClass, JsResult};

use super::internals::{self, Queue, Reaction, chunk_size, promise_call, upon};
use super::transform_stream;
use super::writable_stream::{self, State};

/// Where a controller's write, close and abort algorithms come from.
#[derive(Clone, Trace, Finalize)]
#[boa_gc(unsafe_no_drop)]
pub enum SinkAlgorithms {
    /// Cleared.
    None,
    /// An underlying sink object's `write`, `close` and `abort` (`undefined` where it has none).
    Js { sink: JSValue, write: JSValue, close: JSValue, abort: JSValue },
    /// The writable side of a `TransformStream`.
    Transform { stream: JSValue },
}

#[derive(Trace, Finalize)]
pub struct WritableStreamDefaultController {
    stream: JSValue,
    queue: Queue,
    /// What the queue holds for a close: an object of this controller's own.
    close_sentinel: JSValue,
    started: bool,
    high_water_mark: f64,
    size: JSValue,
    algorithms: SinkAlgorithms,
    abort_controller: JSValue,
}

pub fn controller_of(value: &JSValue) -> ClassRef<WritableStreamDefaultController> {
    value
        .as_class_ref::<WritableStreamDefaultController>()
        .expect("a WritableStreamDefaultController")
}

fn stream_of_controller(global: &JSGlobalObject, controller: &JSValue) -> JsResult<JSValue> {
    Ok(controller_of(controller).borrow(global)?.stream.clone())
}

pub fn started(global: &JSGlobalObject, controller: &JSValue) -> JsResult<bool> {
    Ok(controller_of(controller).borrow(global)?.started)
}

/// A new `AbortController` of the realm, where it has one.
fn new_abort_controller(global: &JSGlobalObject) -> JsResult<JSValue> {
    match global.to_js_value().get(global, "AbortController")? {
        Some(constructor) if constructor.is_callable() => crate::engine::construct(global, &constructor, &[]),
        _ => Ok(JSValue::UNDEFINED),
    }
}

/// "Signal abort on `controller.[[abortController]]` with `reason`".
pub fn signal_abort(global: &JSGlobalObject, controller: &JSValue, reason: JSValue) -> JsResult<()> {
    let abort_controller = controller_of(controller).borrow(global)?.abort_controller.clone();
    if let Some(abort) = abort_controller.get(global, "abort")? {
        abort.call(global, abort_controller, &[reason])?;
    }
    Ok(())
}

/// `SetUpWritableStreamDefaultController`.
pub fn set_up(
    global: &JSGlobalObject,
    stream: &JSValue,
    start: impl FnOnce(&JSValue) -> JsResult<JSValue>,
    algorithms: SinkAlgorithms,
    high_water_mark: f64,
    size: JSValue,
) -> JsResult<JSValue> {
    let controller = bun_js::class::to_js(
        global,
        WritableStreamDefaultController {
            stream: stream.clone(),
            queue: Queue::default(),
            close_sentinel: JSObject::create(global).to_js(),
            started: false,
            high_water_mark,
            size,
            algorithms,
            abort_controller: new_abort_controller(global)?,
        },
    )?;
    writable_stream::stream_of(stream).borrow_mut(global)?.controller = controller.clone();
    let start_result = start(&controller)?;
    let backpressure = get_backpressure(global, &controller)?;
    writable_stream::update_backpressure(global, stream, backpressure)?;
    let start_promise = internals::resolved(global, start_result);
    let on_started: Reaction<JSValue> = |global, controller, _| {
        controller_of(controller).borrow_mut(global)?.started = true;
        advance_queue_if_needed(global, controller)?;
        Ok(JSValue::UNDEFINED)
    };
    let on_failed: Reaction<JSValue> = |global, controller, reason| {
        controller_of(controller).borrow_mut(global)?.started = true;
        let stream = stream_of_controller(global, controller)?;
        writable_stream::deal_with_rejection(global, &stream, reason)?;
        Ok(JSValue::UNDEFINED)
    };
    upon(global, &start_promise, controller.clone(), Some(on_started), Some(on_failed));
    Ok(controller)
}

fn write_algorithm(global: &JSGlobalObject, controller: &JSValue, chunk: JSValue) -> JsResult<JSValue> {
    let algorithms = controller_of(controller).borrow(global)?.algorithms.clone();
    match algorithms {
        SinkAlgorithms::None => Ok(internals::resolved(global, JSValue::UNDEFINED)),
        SinkAlgorithms::Js { sink, write, .. } => promise_call(global, &write, &sink, &[chunk, controller.clone()]),
        SinkAlgorithms::Transform { stream } => transform_stream::sink_write(global, &stream, chunk),
    }
}

fn close_algorithm(global: &JSGlobalObject, controller: &JSValue) -> JsResult<JSValue> {
    let algorithms = controller_of(controller).borrow(global)?.algorithms.clone();
    match algorithms {
        SinkAlgorithms::None => Ok(internals::resolved(global, JSValue::UNDEFINED)),
        SinkAlgorithms::Js { sink, close, .. } => promise_call(global, &close, &sink, &[]),
        SinkAlgorithms::Transform { stream } => transform_stream::sink_close(global, &stream),
    }
}

/// `[[AbortSteps]](reason)`.
pub fn abort_steps(global: &JSGlobalObject, controller: &JSValue, reason: JSValue) -> JsResult<JSValue> {
    let algorithms = controller_of(controller).borrow(global)?.algorithms.clone();
    let result = match algorithms {
        SinkAlgorithms::None => internals::resolved(global, JSValue::UNDEFINED),
        SinkAlgorithms::Js { sink, abort, .. } => promise_call(global, &abort, &sink, &[reason])?,
        SinkAlgorithms::Transform { stream } => transform_stream::sink_abort(global, &stream, reason)?,
    };
    clear_algorithms(global, controller)?;
    Ok(result)
}

/// `[[ErrorSteps]]()`.
pub fn error_steps(global: &JSGlobalObject, controller: &JSValue) -> JsResult<()> {
    controller_of(controller).borrow_mut(global)?.queue.reset();
    Ok(())
}

/// `WritableStreamDefaultControllerClearAlgorithms`.
pub fn clear_algorithms(global: &JSGlobalObject, controller: &JSValue) -> JsResult<()> {
    let c = controller_of(controller);
    let mut c = c.borrow_mut(global)?;
    c.algorithms = SinkAlgorithms::None;
    c.size = JSValue::UNDEFINED;
    Ok(())
}

/// `WritableStreamDefaultControllerClose`.
pub fn close(global: &JSGlobalObject, controller: &JSValue) -> JsResult<()> {
    {
        let c = controller_of(controller);
        let mut c = c.borrow_mut(global)?;
        let sentinel = c.close_sentinel.clone();
        c.queue.enqueue(global, sentinel, 0.0)?;
    }
    advance_queue_if_needed(global, controller)
}

/// `WritableStreamDefaultControllerError`.
pub fn error(global: &JSGlobalObject, controller: &JSValue, reason: JSValue) -> JsResult<()> {
    let stream = stream_of_controller(global, controller)?;
    clear_algorithms(global, controller)?;
    writable_stream::start_erroring(global, &stream, reason)
}

/// `WritableStreamDefaultControllerErrorIfNeeded`.
pub fn error_if_needed(global: &JSGlobalObject, controller: &JSValue, reason: JSValue) -> JsResult<()> {
    let stream = stream_of_controller(global, controller)?;
    if writable_stream::state(global, &stream)? == State::Writable {
        error(global, controller, reason)?;
    }
    Ok(())
}

/// `WritableStreamDefaultControllerGetBackpressure`.
fn get_backpressure(global: &JSGlobalObject, controller: &JSValue) -> JsResult<bool> {
    Ok(desired_size(global, controller)? <= 0.0)
}

/// `WritableStreamDefaultControllerGetChunkSize`.
pub fn get_chunk_size(global: &JSGlobalObject, controller: &JSValue, chunk: &JSValue) -> JsResult<f64> {
    let size = controller_of(controller).borrow(global)?.size.clone();
    match chunk_size(global, &size, chunk) {
        Ok(size) => Ok(size),
        Err(thrown) => {
            let reason = internals::catch(global, thrown)?;
            error_if_needed(global, controller, reason)?;
            Ok(1.0)
        }
    }
}

/// `WritableStreamDefaultControllerGetDesiredSize`.
pub fn desired_size(global: &JSGlobalObject, controller: &JSValue) -> JsResult<f64> {
    let c = controller_of(controller);
    let c = c.borrow(global)?;
    Ok(c.high_water_mark - c.queue.total_size())
}

/// `WritableStreamDefaultControllerWrite`.
pub fn write(global: &JSGlobalObject, controller: &JSValue, chunk: JSValue, size: f64) -> JsResult<()> {
    let queued = controller_of(controller).borrow_mut(global)?.queue.enqueue(global, chunk, size);
    if let Err(thrown) = queued {
        let reason = internals::catch(global, thrown)?;
        return error_if_needed(global, controller, reason);
    }
    let stream = stream_of_controller(global, controller)?;
    if !writable_stream::close_queued_or_in_flight(global, &stream)?
        && writable_stream::state(global, &stream)? == State::Writable
    {
        let backpressure = get_backpressure(global, controller)?;
        writable_stream::update_backpressure(global, &stream, backpressure)?;
    }
    advance_queue_if_needed(global, controller)
}

/// `WritableStreamDefaultControllerAdvanceQueueIfNeeded`.
fn advance_queue_if_needed(global: &JSGlobalObject, controller: &JSValue) -> JsResult<()> {
    let stream = stream_of_controller(global, controller)?;
    if !started(global, controller)? || writable_stream::has_in_flight_write(global, &stream)? {
        return Ok(());
    }
    let state = writable_stream::state(global, &stream)?;
    if state == State::Erroring {
        return writable_stream::finish_erroring(global, &stream);
    }
    if matches!(state, State::Closed | State::Errored) {
        return Ok(());
    }
    let (empty, value, sentinel) = {
        let c = controller_of(controller);
        let c = c.borrow(global)?;
        (c.queue.is_empty(), c.queue.peek(), c.close_sentinel.clone())
    };
    if empty {
        return Ok(());
    }
    if value.is_strict_equal(sentinel, global)? {
        process_close(global, controller)
    } else {
        process_write(global, controller, value)
    }
}

/// `WritableStreamDefaultControllerProcessClose`.
fn process_close(global: &JSGlobalObject, controller: &JSValue) -> JsResult<()> {
    let stream = stream_of_controller(global, controller)?;
    writable_stream::mark_close_request_in_flight(global, &stream)?;
    controller_of(controller).borrow_mut(global)?.queue.dequeue();
    let sink_close = close_algorithm(global, controller)?;
    clear_algorithms(global, controller)?;
    let closed: Reaction<JSValue> = |global, stream, _| {
        writable_stream::finish_in_flight_close(global, stream)?;
        Ok(JSValue::UNDEFINED)
    };
    let failed: Reaction<JSValue> = |global, stream, reason| {
        writable_stream::finish_in_flight_close_with_error(global, stream, reason)?;
        Ok(JSValue::UNDEFINED)
    };
    upon(global, &sink_close, stream, Some(closed), Some(failed));
    Ok(())
}

/// `WritableStreamDefaultControllerProcessWrite`.
fn process_write(global: &JSGlobalObject, controller: &JSValue, chunk: JSValue) -> JsResult<()> {
    let stream = stream_of_controller(global, controller)?;
    writable_stream::mark_first_write_request_in_flight(global, &stream)?;
    let sink_write = write_algorithm(global, controller, chunk)?;
    let written: Reaction<JSValue> = |global, controller, _| {
        let stream = stream_of_controller(global, controller)?;
        writable_stream::finish_in_flight_write(global, &stream)?;
        let state = writable_stream::state(global, &stream)?;
        controller_of(controller).borrow_mut(global)?.queue.dequeue();
        if !writable_stream::close_queued_or_in_flight(global, &stream)? && state == State::Writable {
            let backpressure = get_backpressure(global, controller)?;
            writable_stream::update_backpressure(global, &stream, backpressure)?;
        }
        advance_queue_if_needed(global, controller)?;
        Ok(JSValue::UNDEFINED)
    };
    let failed: Reaction<JSValue> = |global, controller, reason| {
        let stream = stream_of_controller(global, controller)?;
        if writable_stream::state(global, &stream)? == State::Writable {
            clear_algorithms(global, controller)?;
        }
        writable_stream::finish_in_flight_write_with_error(global, &stream, reason)?;
        Ok(JSValue::UNDEFINED)
    };
    upon(global, &sink_write, controller.clone(), Some(written), Some(failed));
    Ok(())
}

impl JsClass for WritableStreamDefaultController {
    const NAME: &'static str = "WritableStreamDefaultController";

    fn define(class: &mut ClassBuilder<'_, Self>) -> JsResult<()> {
        let attributes = Attributes::DEFAULT;
        class.getter("signal", attributes, |this, global| {
            let abort_controller = this.borrow(global)?.abort_controller.clone();
            Ok(abort_controller.get(global, "signal")?.unwrap_or_default())
        })?;
        class.method("error", 1, attributes, |this, global, frame| {
            let controller = this.to_js();
            let stream = stream_of_controller(global, &controller)?;
            if writable_stream::state(global, &stream)? == State::Writable {
                error(global, &controller, frame.argument(0))?;
            }
            Ok(JSValue::UNDEFINED)
        })?;
        Ok(())
    }
}
