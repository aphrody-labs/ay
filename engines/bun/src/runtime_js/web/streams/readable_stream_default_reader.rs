//! `ReadableStreamDefaultReader` (Streams §4.4, §4.9.2): JSReadableStreamDefaultReader.cpp, JSReadableStreamReaderBase.cpp
//! and JSReadRequest.cpp.

use std::collections::VecDeque;

use boa_gc::{Finalize, Trace};
use bun_js::class::{Attributes, ClassBuilder, ClassRef};
use bun_js::{CallFrame, JSGlobalObject, JSValue, JsClass, JsResult};

use super::internals::{self, Deferred, invalid_state, iter_result, mark_handled};
use super::readable_stream::{self, State, stream_of};
use super::readable_stream_default_controller as controller;
use super::{stream_pipe_to_operation, stream_tee_state};

/// What a read does with its chunk, with the end of the stream, or with its error (JSReadRequest's kinds).
#[derive(Clone, Trace, Finalize)]
#[boa_gc(unsafe_no_drop)]
pub enum ReadRequest {
    /// `reader.read()`: a promise of `{ value, done }`.
    Promise(Deferred),
    /// A pull of `tee()`.
    Tee(JSValue),
    /// The next step of an async iterator: the iterator's reader, and the promise of its next result.
    Iterator { reader: JSValue, deferred: Deferred },
    /// A read of `pipeTo()`.
    Pipe(JSValue),
    /// A read of `text()`, `bytes()`, `json()`, `blob()` and `arrayBuffer()`.
    Consume(JSValue),
}

impl ReadRequest {
    pub fn chunk_steps(self, global: &JSGlobalObject, chunk: JSValue) -> JsResult<()> {
        match self {
            ReadRequest::Promise(deferred) => deferred.resolve(global, iter_result(global, chunk, false)),
            ReadRequest::Tee(state) => return stream_tee_state::chunk_steps(global, &state, chunk),
            ReadRequest::Iterator { deferred, .. } => deferred.resolve(global, iter_result(global, chunk, false)),
            ReadRequest::Pipe(operation) => return stream_pipe_to_operation::chunk_steps(global, &operation, chunk),
            ReadRequest::Consume(consumer) => {
                return super::readable_stream::consume_chunk_steps(global, &consumer, chunk);
            }
        }
        Ok(())
    }

    pub fn close_steps(self, global: &JSGlobalObject) -> JsResult<()> {
        match self {
            ReadRequest::Promise(deferred) => deferred.resolve(global, iter_result(global, JSValue::UNDEFINED, true)),
            ReadRequest::Tee(state) => return stream_tee_state::close_steps(global, &state),
            ReadRequest::Iterator { reader, deferred } => {
                release(global, &reader)?;
                deferred.resolve(global, iter_result(global, JSValue::UNDEFINED, true));
            }
            ReadRequest::Pipe(_) => {}
            ReadRequest::Consume(consumer) => return super::readable_stream::consume_close_steps(global, &consumer),
        }
        Ok(())
    }

    pub fn error_steps(self, global: &JSGlobalObject, reason: JSValue) -> JsResult<()> {
        match self {
            ReadRequest::Promise(deferred) => deferred.reject(global, reason),
            ReadRequest::Tee(state) => return stream_tee_state::error_steps(global, &state),
            ReadRequest::Iterator { reader, deferred } => {
                release(global, &reader)?;
                deferred.reject(global, reason);
            }
            ReadRequest::Pipe(_) => {}
            ReadRequest::Consume(consumer) => {
                return super::readable_stream::consume_error_steps(global, &consumer, reason);
            }
        }
        Ok(())
    }
}

#[derive(Trace, Finalize)]
pub struct ReadableStreamDefaultReader {
    pub(crate) stream: JSValue,
    pub(crate) closed: Deferred,
    pub(crate) read_requests: VecDeque<ReadRequest>,
}

pub fn reader_of(value: &JSValue) -> ClassRef<ReadableStreamDefaultReader> {
    value
        .as_class_ref::<ReadableStreamDefaultReader>()
        .expect("a ReadableStreamDefaultReader")
}

/// `ReadableStreamReaderGenericInitialize` and `SetUpReadableStreamDefaultReader`, on a new reader object.
pub fn set_up(global: &JSGlobalObject, reader: &JSValue, stream: &JSValue) -> JsResult<()> {
    if readable_stream::is_locked(global, stream)? {
        return Err(global.throw_value(invalid_state(global, "ReadableStream is locked")));
    }
    let (state, stored_error) = {
        let s = stream_of(stream);
        let mut s = s.borrow_mut(global)?;
        s.reader = reader.clone();
        (s.state, s.stored_error.clone())
    };
    let closed = match state {
        State::Readable => Deferred::new(global),
        State::Closed => {
            let deferred = Deferred::new(global);
            deferred.resolve(global, JSValue::UNDEFINED);
            deferred
        }
        State::Errored => Deferred::rejected_handled(global, stored_error),
    };
    let r = reader_of(reader);
    let mut r = r.borrow_mut(global)?;
    r.stream = stream.clone();
    r.closed = closed;
    r.read_requests.clear();
    Ok(())
}

/// `AcquireReadableStreamDefaultReader`.
pub fn acquire(global: &JSGlobalObject, stream: &JSValue) -> JsResult<JSValue> {
    let reader = bun_js::class::to_js(global, ReadableStreamDefaultReader::detached(global))?;
    set_up(global, &reader, stream)?;
    Ok(reader)
}

impl ReadableStreamDefaultReader {
    fn detached(global: &JSGlobalObject) -> ReadableStreamDefaultReader {
        ReadableStreamDefaultReader {
            stream: JSValue::UNDEFINED,
            closed: Deferred::new(global),
            read_requests: VecDeque::new(),
        }
    }
}

/// `ReadableStreamDefaultReaderRead`.
pub fn read(global: &JSGlobalObject, reader: &JSValue, read_request: ReadRequest) -> JsResult<()> {
    let stream = reader_of(reader).borrow(global)?.stream.clone();
    let (state, stored_error, controller) = {
        let s = stream_of(&stream);
        let mut s = s.borrow_mut(global)?;
        s.disturbed = true;
        (s.state, s.stored_error.clone(), s.controller.clone())
    };
    match state {
        State::Closed => read_request.close_steps(global),
        State::Errored => read_request.error_steps(global, stored_error),
        State::Readable if super::readable_byte_stream_controller::is_byte_controller(&controller) => {
            super::readable_byte_stream_controller::pull_steps(global, &controller, read_request)
        }
        State::Readable => controller::pull_steps(global, &controller, read_request),
    }
}

/// `ReadableStreamReaderGenericRelease`.
fn generic_release(global: &JSGlobalObject, reader: &JSValue) -> JsResult<()> {
    let (stream, closed) = {
        let r = reader_of(reader);
        let r = r.borrow(global)?;
        (r.stream.clone(), r.closed.clone())
    };
    let released = invalid_state(global, "Reader released");
    if readable_stream::state(global, &stream)? == State::Readable {
        closed.reject(global, released);
        mark_handled(global, &closed.promise);
    } else {
        reader_of(reader).borrow_mut(global)?.closed = Deferred::rejected_handled(global, released);
    }
    let controller = stream_of(&stream).borrow(global)?.controller.clone();
    if super::readable_byte_stream_controller::is_byte_controller(&controller) {
        super::readable_byte_stream_controller::release_steps(global, &controller)?;
    }
    stream_of(&stream).borrow_mut(global)?.reader = JSValue::UNDEFINED;
    reader_of(reader).borrow_mut(global)?.stream = JSValue::UNDEFINED;
    Ok(())
}

/// `ReadableStreamDefaultReaderErrorReadRequests`.
pub fn error_read_requests(global: &JSGlobalObject, reader: &JSValue, reason: JSValue) -> JsResult<()> {
    let requests = std::mem::take(&mut reader_of(reader).borrow_mut(global)?.read_requests);
    for request in requests {
        request.error_steps(global, reason.clone())?;
    }
    Ok(())
}

/// `ReadableStreamDefaultReaderRelease`.
pub fn release(global: &JSGlobalObject, reader: &JSValue) -> JsResult<()> {
    generic_release(global, reader)?;
    error_read_requests(global, reader, invalid_state(global, "Releasing reader"))
}

impl JsClass for ReadableStreamDefaultReader {
    const NAME: &'static str = "ReadableStreamDefaultReader";
    const LENGTH: usize = 1;

    fn construct(global: &JSGlobalObject, frame: &CallFrame<'_>) -> JsResult<Self> {
        if frame.argument(0).as_class_ref::<readable_stream::ReadableStream>().is_none() {
            return Err(global.throw_type_error(format_args!(
                "ReadableStreamDefaultReader constructor requires a ReadableStream as its first argument"
            )));
        }
        Ok(ReadableStreamDefaultReader::detached(global))
    }

    fn initialize(this: &ClassRef<Self>, global: &JSGlobalObject, frame: &CallFrame<'_>) -> JsResult<()> {
        set_up(global, &this.to_js(), &frame.argument(0))
    }

    fn define(class: &mut ClassBuilder<'_, Self>) -> JsResult<()> {
        let attributes = Attributes::DEFAULT;
        class.getter("closed", attributes, |this, global| Ok(this.borrow(global)?.closed.promise.clone()))?;
        class.method("cancel", 1, attributes, |this, global, frame| {
            let stream = this.borrow(global)?.stream.clone();
            if stream.is_undefined() {
                let error = invalid_state(global, "The reader is not attached to a stream");
                return Ok(internals::rejected(global, error));
            }
            readable_stream::cancel(global, &stream, frame.argument(0))
        })?;
        class.method("read", 0, attributes, |this, global, _| {
            if this.borrow(global)?.stream.is_undefined() {
                let error = invalid_state(global, "The reader is not attached to a stream");
                return Ok(internals::rejected(global, error));
            }
            let deferred = Deferred::new(global);
            read(global, &this.to_js(), ReadRequest::Promise(deferred.clone()))?;
            Ok(deferred.promise)
        })?;
        class.method("releaseLock", 0, attributes, |this, global, _| {
            if !this.borrow(global)?.stream.is_undefined() {
                release(global, &this.to_js())?;
            }
            Ok(JSValue::UNDEFINED)
        })?;
        Ok(())
    }
}
