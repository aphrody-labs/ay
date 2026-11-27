//! `WritableStream` (Streams §5.2, §5.5.1, §5.5.2): JSWritableStream.cpp and WritableStreamOperations.cpp.

use std::collections::VecDeque;

use boa_gc::{Finalize, Trace};
use bun_js::class::{Attributes, ClassBuilder, ClassRef};
use bun_js::{CallFrame, JSGlobalObject, JSValue, JsClass, JsResult};

use super::internals::{self, Deferred, Reaction, callback_member, invalid_state, member, upon};
use super::writable_stream_default_controller::{self as controller, SinkAlgorithms};
use super::writable_stream_default_writer::{self as writer, writer_of};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum State {
    Writable,
    Closed,
    Erroring,
    Errored,
}

#[derive(Clone, Trace, Finalize)]
#[boa_gc(unsafe_no_drop)]
pub struct PendingAbort {
    deferred: Deferred,
    reason: JSValue,
    was_already_erroring: bool,
}

#[derive(Trace, Finalize)]
pub struct WritableStream {
    #[unsafe_ignore_trace]
    pub(crate) state: State,
    pub(crate) stored_error: JSValue,
    pub(crate) writer: JSValue,
    pub(crate) controller: JSValue,
    pub(crate) backpressure: bool,
    close_request: Option<Deferred>,
    in_flight_close_request: Option<Deferred>,
    in_flight_write_request: Option<Deferred>,
    pending_abort: Option<PendingAbort>,
    write_requests: VecDeque<Deferred>,
}

impl WritableStream {
    /// `InitializeWritableStream`.
    fn initial() -> WritableStream {
        WritableStream {
            state: State::Writable,
            stored_error: JSValue::UNDEFINED,
            writer: JSValue::UNDEFINED,
            controller: JSValue::UNDEFINED,
            backpressure: false,
            close_request: None,
            in_flight_close_request: None,
            in_flight_write_request: None,
            pending_abort: None,
            write_requests: VecDeque::new(),
        }
    }
}

pub fn stream_of(value: &JSValue) -> ClassRef<WritableStream> {
    value.as_class_ref::<WritableStream>().expect("a WritableStream")
}

pub fn is_writable_stream(value: &JSValue) -> bool {
    value.as_class_ref::<WritableStream>().is_some()
}

pub fn state(global: &JSGlobalObject, stream: &JSValue) -> JsResult<State> {
    Ok(stream_of(stream).borrow(global)?.state)
}

pub fn stored_error(global: &JSGlobalObject, stream: &JSValue) -> JsResult<JSValue> {
    Ok(stream_of(stream).borrow(global)?.stored_error.clone())
}

/// `IsWritableStreamLocked`.
pub fn is_locked(global: &JSGlobalObject, stream: &JSValue) -> JsResult<bool> {
    Ok(!stream_of(stream).borrow(global)?.writer.is_undefined())
}

/// `CreateWritableStream` with Rust algorithms. `start` is what the start algorithm returns.
pub fn create(
    global: &JSGlobalObject,
    start: JSValue,
    algorithms: SinkAlgorithms,
    high_water_mark: f64,
    size: JSValue,
) -> JsResult<JSValue> {
    let stream = bun_js::class::to_js(global, WritableStream::initial())?;
    controller::set_up(global, &stream, move |_| Ok(start), algorithms, high_water_mark, size)?;
    Ok(stream)
}

/// `WritableStreamAbort`.
pub fn abort(global: &JSGlobalObject, stream: &JSValue, reason: JSValue) -> JsResult<JSValue> {
    let (state, controller) = {
        let s = stream_of(stream);
        let s = s.borrow(global)?;
        (s.state, s.controller.clone())
    };
    if matches!(state, State::Closed | State::Errored) {
        return Ok(internals::resolved(global, JSValue::UNDEFINED));
    }
    controller::signal_abort(global, &controller, reason.clone())?;
    let s = stream_of(stream);
    let mut s = s.borrow_mut(global)?;
    if matches!(s.state, State::Closed | State::Errored) {
        return Ok(internals::resolved(global, JSValue::UNDEFINED));
    }
    if let Some(pending) = &s.pending_abort {
        return Ok(pending.deferred.promise.clone());
    }
    let was_already_erroring = s.state == State::Erroring;
    let reason = if was_already_erroring { JSValue::UNDEFINED } else { reason };
    let deferred = Deferred::new(global);
    s.pending_abort = Some(PendingAbort {
        deferred: deferred.clone(),
        reason: reason.clone(),
        was_already_erroring,
    });
    drop(s);
    if !was_already_erroring {
        start_erroring(global, stream, reason)?;
    }
    Ok(deferred.promise)
}

/// `WritableStreamClose`.
pub fn close(global: &JSGlobalObject, stream: &JSValue) -> JsResult<JSValue> {
    let (state, writer, backpressure, controller) = {
        let s = stream_of(stream);
        let s = s.borrow(global)?;
        (s.state, s.writer.clone(), s.backpressure, s.controller.clone())
    };
    if matches!(state, State::Closed | State::Errored) {
        let error = invalid_state(global, "Cannot close a WritableStream that is closed or errored");
        return Ok(internals::rejected(global, error));
    }
    let deferred = Deferred::new(global);
    stream_of(stream).borrow_mut(global)?.close_request = Some(deferred.clone());
    if !writer.is_undefined() && backpressure && state == State::Writable {
        writer_of(&writer).borrow(global)?.ready.resolve(global, JSValue::UNDEFINED);
    }
    controller::close(global, &controller)?;
    Ok(deferred.promise)
}

/// `WritableStreamAddWriteRequest`.
pub fn add_write_request(global: &JSGlobalObject, stream: &JSValue) -> JsResult<JSValue> {
    let deferred = Deferred::new(global);
    stream_of(stream).borrow_mut(global)?.write_requests.push_back(deferred.clone());
    Ok(deferred.promise)
}

/// `WritableStreamCloseQueuedOrInFlight`.
pub fn close_queued_or_in_flight(global: &JSGlobalObject, stream: &JSValue) -> JsResult<bool> {
    let s = stream_of(stream);
    let s = s.borrow(global)?;
    Ok(s.close_request.is_some() || s.in_flight_close_request.is_some())
}

/// `WritableStreamDealWithRejection`.
pub fn deal_with_rejection(global: &JSGlobalObject, stream: &JSValue, error: JSValue) -> JsResult<()> {
    if state(global, stream)? == State::Writable {
        return start_erroring(global, stream, error);
    }
    finish_erroring(global, stream)
}

/// `WritableStreamFinishErroring`.
pub fn finish_erroring(global: &JSGlobalObject, stream: &JSValue) -> JsResult<()> {
    let (controller, stored_error, requests) = {
        let s = stream_of(stream);
        let mut s = s.borrow_mut(global)?;
        s.state = State::Errored;
        (s.controller.clone(), s.stored_error.clone(), std::mem::take(&mut s.write_requests))
    };
    controller::error_steps(global, &controller)?;
    for request in requests {
        request.reject(global, stored_error.clone());
    }
    let pending = stream_of(stream).borrow_mut(global)?.pending_abort.take();
    let Some(pending) = pending else {
        return reject_close_and_closed_promise_if_needed(global, stream);
    };
    if pending.was_already_erroring {
        pending.deferred.reject(global, stored_error);
        return reject_close_and_closed_promise_if_needed(global, stream);
    }
    let promise = controller::abort_steps(global, &controller, pending.reason.clone())?;
    #[derive(Clone, Trace, Finalize)]
    #[boa_gc(unsafe_no_drop)]
    struct Abort {
        stream: JSValue,
        deferred: Deferred,
    }
    let fulfilled: Reaction<Abort> = |global, abort, _| {
        abort.deferred.resolve(global, JSValue::UNDEFINED);
        reject_close_and_closed_promise_if_needed(global, &abort.stream)?;
        Ok(JSValue::UNDEFINED)
    };
    let rejected: Reaction<Abort> = |global, abort, reason| {
        abort.deferred.reject(global, reason);
        reject_close_and_closed_promise_if_needed(global, &abort.stream)?;
        Ok(JSValue::UNDEFINED)
    };
    let abort = Abort {
        stream: stream.clone(),
        deferred: pending.deferred,
    };
    upon(global, &promise, abort, Some(fulfilled), Some(rejected));
    Ok(())
}

/// `WritableStreamFinishInFlightClose`.
pub fn finish_in_flight_close(global: &JSGlobalObject, stream: &JSValue) -> JsResult<()> {
    let (close, pending, writer) = {
        let s = stream_of(stream);
        let mut s = s.borrow_mut(global)?;
        let close = s.in_flight_close_request.take();
        let mut pending = None;
        if s.state == State::Erroring {
            s.stored_error = JSValue::UNDEFINED;
            pending = s.pending_abort.take();
        }
        s.state = State::Closed;
        (close, pending, s.writer.clone())
    };
    if let Some(close) = close {
        close.resolve(global, JSValue::UNDEFINED);
    }
    if let Some(pending) = pending {
        pending.deferred.resolve(global, JSValue::UNDEFINED);
    }
    if !writer.is_undefined() {
        writer_of(&writer).borrow(global)?.closed.resolve(global, JSValue::UNDEFINED);
    }
    Ok(())
}

/// `WritableStreamFinishInFlightCloseWithError`.
pub fn finish_in_flight_close_with_error(global: &JSGlobalObject, stream: &JSValue, error: JSValue) -> JsResult<()> {
    let (close, pending) = {
        let s = stream_of(stream);
        let mut s = s.borrow_mut(global)?;
        (s.in_flight_close_request.take(), s.pending_abort.take())
    };
    if let Some(close) = close {
        close.reject(global, error.clone());
    }
    if let Some(pending) = pending {
        pending.deferred.reject(global, error.clone());
    }
    deal_with_rejection(global, stream, error)
}

/// `WritableStreamFinishInFlightWrite`.
pub fn finish_in_flight_write(global: &JSGlobalObject, stream: &JSValue) -> JsResult<()> {
    let write = stream_of(stream).borrow_mut(global)?.in_flight_write_request.take();
    if let Some(write) = write {
        write.resolve(global, JSValue::UNDEFINED);
    }
    Ok(())
}

/// `WritableStreamFinishInFlightWriteWithError`.
pub fn finish_in_flight_write_with_error(global: &JSGlobalObject, stream: &JSValue, error: JSValue) -> JsResult<()> {
    let write = stream_of(stream).borrow_mut(global)?.in_flight_write_request.take();
    if let Some(write) = write {
        write.reject(global, error.clone());
    }
    deal_with_rejection(global, stream, error)
}

/// `WritableStreamHasOperationMarkedInFlight`.
pub fn has_operation_marked_in_flight(global: &JSGlobalObject, stream: &JSValue) -> JsResult<bool> {
    let s = stream_of(stream);
    let s = s.borrow(global)?;
    Ok(s.in_flight_write_request.is_some() || s.in_flight_close_request.is_some())
}

pub fn has_in_flight_write(global: &JSGlobalObject, stream: &JSValue) -> JsResult<bool> {
    Ok(stream_of(stream).borrow(global)?.in_flight_write_request.is_some())
}

/// `WritableStreamMarkCloseRequestInFlight`.
pub fn mark_close_request_in_flight(global: &JSGlobalObject, stream: &JSValue) -> JsResult<()> {
    let s = stream_of(stream);
    let mut s = s.borrow_mut(global)?;
    s.in_flight_close_request = s.close_request.take();
    Ok(())
}

/// `WritableStreamMarkFirstWriteRequestInFlight`.
pub fn mark_first_write_request_in_flight(global: &JSGlobalObject, stream: &JSValue) -> JsResult<()> {
    let s = stream_of(stream);
    let mut s = s.borrow_mut(global)?;
    s.in_flight_write_request = s.write_requests.pop_front();
    Ok(())
}

/// `WritableStreamRejectCloseAndClosedPromiseIfNeeded`.
fn reject_close_and_closed_promise_if_needed(global: &JSGlobalObject, stream: &JSValue) -> JsResult<()> {
    let (close, stored_error, writer) = {
        let s = stream_of(stream);
        let mut s = s.borrow_mut(global)?;
        (s.close_request.take(), s.stored_error.clone(), s.writer.clone())
    };
    if let Some(close) = close {
        close.reject(global, stored_error.clone());
    }
    if !writer.is_undefined() {
        let closed = writer_of(&writer).borrow(global)?.closed.clone();
        closed.reject(global, stored_error);
        internals::mark_handled(global, &closed.promise);
    }
    Ok(())
}

/// `WritableStreamStartErroring`.
pub fn start_erroring(global: &JSGlobalObject, stream: &JSValue, reason: JSValue) -> JsResult<()> {
    let (controller, writer) = {
        let s = stream_of(stream);
        let mut s = s.borrow_mut(global)?;
        s.state = State::Erroring;
        s.stored_error = reason.clone();
        (s.controller.clone(), s.writer.clone())
    };
    if !writer.is_undefined() {
        writer::ensure_ready_promise_rejected(global, &writer, reason)?;
    }
    if !has_operation_marked_in_flight(global, stream)? && controller::started(global, &controller)? {
        finish_erroring(global, stream)?;
    }
    Ok(())
}

/// `WritableStreamUpdateBackpressure`.
pub fn update_backpressure(global: &JSGlobalObject, stream: &JSValue, backpressure: bool) -> JsResult<()> {
    let (writer, current) = {
        let s = stream_of(stream);
        let s = s.borrow(global)?;
        (s.writer.clone(), s.backpressure)
    };
    if !writer.is_undefined() && backpressure != current {
        let w = writer_of(&writer);
        if backpressure {
            w.borrow_mut(global)?.ready = Deferred::new(global);
        } else {
            w.borrow(global)?.ready.resolve(global, JSValue::UNDEFINED);
        }
    }
    stream_of(stream).borrow_mut(global)?.backpressure = backpressure;
    Ok(())
}

/// The WritableStream constructor's steps, on the new stream.
fn construct_steps(global: &JSGlobalObject, stream: &JSValue, frame: &CallFrame<'_>) -> JsResult<()> {
    let [underlying_sink, strategy] = frame.arguments_as_array::<2>();
    if !underlying_sink.is_undefined_or_null() && !underlying_sink.is_object() {
        return Err(global.throw_type_error(format_args!(
            "WritableStream constructor takes an object as first argument"
        )));
    }
    // The UnderlyingSink dictionary, its members in order.
    let what = "The underlying sink";
    let abort = callback_member(global, &underlying_sink, "abort", what)?;
    let close = callback_member(global, &underlying_sink, "close", what)?;
    let start = callback_member(global, &underlying_sink, "start", what)?;
    let kind = member(global, &underlying_sink, "type")?;
    let write = callback_member(global, &underlying_sink, "write", what)?;
    let strategy = internals::strategy(
        global,
        &strategy,
        "WritableStream constructor takes an object as second argument, if any",
    )?;
    if !kind.is_undefined() {
        return Err(global.throw_range_error(format_args!(
            "The underlying sink's 'type' property is reserved and must not be present"
        )));
    }
    let high_water_mark = internals::extract_high_water_mark(global, &strategy, 1.0)?;
    let algorithms = SinkAlgorithms::Js {
        sink: underlying_sink.clone(),
        write,
        close,
        abort,
    };
    controller::set_up(
        global,
        stream,
        move |controller| {
            if start.is_callable() {
                start.call(global, underlying_sink, std::slice::from_ref(controller))
            } else {
                Ok(JSValue::UNDEFINED)
            }
        },
        algorithms,
        high_water_mark,
        strategy.size,
    )?;
    Ok(())
}

impl JsClass for WritableStream {
    const NAME: &'static str = "WritableStream";

    fn construct(_: &JSGlobalObject, _: &CallFrame<'_>) -> JsResult<Self> {
        Ok(WritableStream::initial())
    }

    fn initialize(this: &ClassRef<Self>, global: &JSGlobalObject, frame: &CallFrame<'_>) -> JsResult<()> {
        construct_steps(global, &this.to_js(), frame)
    }

    fn define(class: &mut ClassBuilder<'_, Self>) -> JsResult<()> {
        let attributes = Attributes::DEFAULT;
        class.getter("locked", attributes, |this, global| {
            Ok(JSValue::js_boolean(!this.borrow(global)?.writer.is_undefined()))
        })?;
        class.method("abort", 1, attributes, |this, global, frame| {
            let stream = this.to_js();
            if is_locked(global, &stream)? {
                let error = invalid_state(global, "Cannot abort a locked WritableStream");
                return Ok(internals::rejected(global, error));
            }
            abort(global, &stream, frame.argument(0))
        })?;
        class.method("close", 0, attributes, |this, global, _| {
            let stream = this.to_js();
            if is_locked(global, &stream)? {
                let error = invalid_state(global, "Cannot close a locked WritableStream");
                return Ok(internals::rejected(global, error));
            }
            if close_queued_or_in_flight(global, &stream)? {
                let error = invalid_state(global, "Cannot close a WritableStream that is already closing");
                return Ok(internals::rejected(global, error));
            }
            close(global, &stream)
        })?;
        class.method("getWriter", 0, attributes, |this, global, _| writer::acquire(global, &this.to_js()))?;
        Ok(())
    }
}
