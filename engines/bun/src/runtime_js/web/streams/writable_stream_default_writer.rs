//! `WritableStreamDefaultWriter` (Streams §5.3, §5.5.3): JSWritableStreamDefaultWriter.cpp.

use boa_gc::{Finalize, Trace};
use bun_js::class::{Attributes, ClassBuilder, ClassRef};
use bun_js::{CallFrame, JSGlobalObject, JSValue, JsClass, JsResult};

use super::internals::{self, Deferred, invalid_state, mark_handled};
use super::writable_stream::{self, State, stream_of};
use super::writable_stream_default_controller as controller;

#[derive(Trace, Finalize)]
pub struct WritableStreamDefaultWriter {
    pub(crate) stream: JSValue,
    pub(crate) ready: Deferred,
    pub(crate) closed: Deferred,
}

pub fn writer_of(value: &JSValue) -> ClassRef<WritableStreamDefaultWriter> {
    value
        .as_class_ref::<WritableStreamDefaultWriter>()
        .expect("a WritableStreamDefaultWriter")
}

fn resolved_deferred(global: &JSGlobalObject) -> Deferred {
    let deferred = Deferred::new(global);
    deferred.resolve(global, JSValue::UNDEFINED);
    deferred
}

impl WritableStreamDefaultWriter {
    fn detached(global: &JSGlobalObject) -> WritableStreamDefaultWriter {
        WritableStreamDefaultWriter {
            stream: JSValue::UNDEFINED,
            ready: Deferred::new(global),
            closed: Deferred::new(global),
        }
    }
}

/// `SetUpWritableStreamDefaultWriter`, on a new writer object.
pub fn set_up(global: &JSGlobalObject, writer: &JSValue, stream: &JSValue) -> JsResult<()> {
    if writable_stream::is_locked(global, stream)? {
        return Err(global.throw_value(invalid_state(global, "WritableStream is locked")));
    }
    let (state, stored_error, backpressure) = {
        let s = stream_of(stream);
        let mut s = s.borrow_mut(global)?;
        s.writer = writer.clone();
        (s.state, s.stored_error.clone(), s.backpressure)
    };
    let (ready, closed) = match state {
        State::Writable => {
            let ready = if !writable_stream::close_queued_or_in_flight(global, stream)? && backpressure {
                Deferred::new(global)
            } else {
                resolved_deferred(global)
            };
            (ready, Deferred::new(global))
        }
        State::Erroring => (Deferred::rejected_handled(global, stored_error), Deferred::new(global)),
        State::Closed => (resolved_deferred(global), resolved_deferred(global)),
        State::Errored => (
            Deferred::rejected_handled(global, stored_error.clone()),
            Deferred::rejected_handled(global, stored_error),
        ),
    };
    let w = writer_of(writer);
    let mut w = w.borrow_mut(global)?;
    w.stream = stream.clone();
    w.ready = ready;
    w.closed = closed;
    Ok(())
}

/// `AcquireWritableStreamDefaultWriter`.
pub fn acquire(global: &JSGlobalObject, stream: &JSValue) -> JsResult<JSValue> {
    let writer = bun_js::class::to_js(global, WritableStreamDefaultWriter::detached(global))?;
    set_up(global, &writer, stream)?;
    Ok(writer)
}

/// `WritableStreamDefaultWriterEnsureReadyPromiseRejected`.
pub fn ensure_ready_promise_rejected(global: &JSGlobalObject, writer: &JSValue, error: JSValue) -> JsResult<()> {
    let ready = writer_of(writer).borrow(global)?.ready.clone();
    if crate::engine::is_pending(&ready.promise) {
        ready.reject(global, error);
        mark_handled(global, &ready.promise);
    } else {
        writer_of(writer).borrow_mut(global)?.ready = Deferred::rejected_handled(global, error);
    }
    Ok(())
}

/// `WritableStreamDefaultWriterEnsureClosedPromiseRejected`.
fn ensure_closed_promise_rejected(global: &JSGlobalObject, writer: &JSValue, error: JSValue) -> JsResult<()> {
    let closed = writer_of(writer).borrow(global)?.closed.clone();
    if crate::engine::is_pending(&closed.promise) {
        closed.reject(global, error);
        mark_handled(global, &closed.promise);
    } else {
        writer_of(writer).borrow_mut(global)?.closed = Deferred::rejected_handled(global, error);
    }
    Ok(())
}

/// `WritableStreamDefaultWriterGetDesiredSize`: `None` for an errored or erroring stream.
fn desired_size(global: &JSGlobalObject, stream: &JSValue) -> JsResult<Option<f64>> {
    let (state, controller) = {
        let s = stream_of(stream);
        let s = s.borrow(global)?;
        (s.state, s.controller.clone())
    };
    Ok(match state {
        State::Errored | State::Erroring => None,
        State::Closed => Some(0.0),
        State::Writable => Some(controller::desired_size(global, &controller)?),
    })
}

/// `WritableStreamDefaultWriterRelease`.
pub fn release(global: &JSGlobalObject, writer: &JSValue) -> JsResult<()> {
    let stream = writer_of(writer).borrow(global)?.stream.clone();
    let released = invalid_state(global, "Writer has been released");
    ensure_ready_promise_rejected(global, writer, released.clone())?;
    ensure_closed_promise_rejected(global, writer, released)?;
    stream_of(&stream).borrow_mut(global)?.writer = JSValue::UNDEFINED;
    writer_of(writer).borrow_mut(global)?.stream = JSValue::UNDEFINED;
    Ok(())
}

/// `WritableStreamDefaultWriterWrite`.
pub fn write(global: &JSGlobalObject, writer: &JSValue, chunk: JSValue) -> JsResult<JSValue> {
    let stream = writer_of(writer).borrow(global)?.stream.clone();
    let controller = stream_of(&stream).borrow(global)?.controller.clone();
    let size = controller::get_chunk_size(global, &controller, &chunk)?;
    let current = writer_of(writer).borrow(global)?.stream.clone();
    if !current.is_strict_equal(stream.clone(), global)? {
        let error = internals::type_error(
            global,
            "This WritableStreamDefaultWriter was released while the queuing strategy's size() was running",
        );
        return Ok(internals::rejected(global, error));
    }
    let (state, stored_error) = {
        let s = stream_of(&stream);
        let s = s.borrow(global)?;
        (s.state, s.stored_error.clone())
    };
    if state == State::Errored {
        return Ok(internals::rejected(global, stored_error));
    }
    if writable_stream::close_queued_or_in_flight(global, &stream)? || state == State::Closed {
        let error = internals::type_error(global, "Cannot write to a WritableStream that is closing or closed");
        return Ok(internals::rejected(global, error));
    }
    if state == State::Erroring {
        return Ok(internals::rejected(global, stored_error));
    }
    let promise = writable_stream::add_write_request(global, &stream)?;
    controller::write(global, &controller, chunk, size)?;
    Ok(promise)
}

/// `WritableStreamDefaultWriterCloseWithErrorPropagation`.
pub fn close_with_error_propagation(global: &JSGlobalObject, writer: &JSValue) -> JsResult<JSValue> {
    let stream = writer_of(writer).borrow(global)?.stream.clone();
    let (state, stored_error) = {
        let s = stream_of(&stream);
        let s = s.borrow(global)?;
        (s.state, s.stored_error.clone())
    };
    if writable_stream::close_queued_or_in_flight(global, &stream)? || state == State::Closed {
        return Ok(internals::resolved(global, JSValue::UNDEFINED));
    }
    if state == State::Errored {
        return Ok(internals::rejected(global, stored_error));
    }
    writable_stream::close(global, &stream)
}

fn unbound(global: &JSGlobalObject) -> JSValue {
    invalid_state(global, "Writer is not bound to a WritableStream")
}

impl JsClass for WritableStreamDefaultWriter {
    const NAME: &'static str = "WritableStreamDefaultWriter";
    const LENGTH: usize = 1;

    fn construct(global: &JSGlobalObject, frame: &CallFrame<'_>) -> JsResult<Self> {
        if !writable_stream::is_writable_stream(&frame.argument(0)) {
            return Err(global.throw_type_error(format_args!(
                "WritableStreamDefaultWriter constructor requires a WritableStream as its first argument"
            )));
        }
        Ok(WritableStreamDefaultWriter::detached(global))
    }

    fn initialize(this: &ClassRef<Self>, global: &JSGlobalObject, frame: &CallFrame<'_>) -> JsResult<()> {
        set_up(global, &this.to_js(), &frame.argument(0))
    }

    fn define(class: &mut ClassBuilder<'_, Self>) -> JsResult<()> {
        let attributes = Attributes::DEFAULT;
        class.getter("closed", attributes, |this, global| Ok(this.borrow(global)?.closed.promise.clone()))?;
        class.getter("desiredSize", attributes, |this, global| {
            let stream = this.borrow(global)?.stream.clone();
            if stream.is_undefined() {
                return Err(global.throw_value(unbound(global)));
            }
            Ok(match desired_size(global, &stream)? {
                Some(size) => JSValue::js_number(size),
                None => JSValue::NULL,
            })
        })?;
        class.getter("ready", attributes, |this, global| Ok(this.borrow(global)?.ready.promise.clone()))?;
        class.method("abort", 1, attributes, |this, global, frame| {
            let stream = this.borrow(global)?.stream.clone();
            if stream.is_undefined() {
                return Ok(internals::rejected(global, unbound(global)));
            }
            writable_stream::abort(global, &stream, frame.argument(0))
        })?;
        class.method("close", 0, attributes, |this, global, _| {
            let stream = this.borrow(global)?.stream.clone();
            if stream.is_undefined() {
                return Ok(internals::rejected(global, unbound(global)));
            }
            if writable_stream::close_queued_or_in_flight(global, &stream)? {
                let error = invalid_state(global, "Cannot close a WritableStream that is already closing");
                return Ok(internals::rejected(global, error));
            }
            writable_stream::close(global, &stream)
        })?;
        class.method("releaseLock", 0, attributes, |this, global, _| {
            if !this.borrow(global)?.stream.is_undefined() {
                release(global, &this.to_js())?;
            }
            Ok(JSValue::UNDEFINED)
        })?;
        class.method("write", 0, attributes, |this, global, frame| {
            if this.borrow(global)?.stream.is_undefined() {
                return Ok(internals::rejected(global, unbound(global)));
            }
            write(global, &this.to_js(), frame.argument(0))
        })?;
        Ok(())
    }
}
