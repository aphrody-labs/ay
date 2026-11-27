//! `ReadableStreamBYOBReader` (Streams §4.5, §4.9.2): JSReadableStreamBYOBReader.cpp.

use std::collections::VecDeque;

use boa_gc::{Finalize, Trace};
use bun_js::class::{Attributes, ClassBuilder, ClassRef};
use bun_js::{CallFrame, JSGlobalObject, JSValue, JsClass, JsResult};

use super::internals::{self, Deferred, invalid_state, iter_result, mark_handled, member};
use super::readable_byte_stream_controller as byte_controller;
use super::readable_stream::{self, State, stream_of};

/// What a `read(view)` does with the filled view, with the end, or with an error.
#[derive(Clone, Trace, Finalize)]
#[boa_gc(unsafe_no_drop)]
pub enum ReadIntoRequest {
    Promise(Deferred),
}

impl ReadIntoRequest {
    pub fn chunk_steps(self, global: &JSGlobalObject, chunk: JSValue) -> JsResult<()> {
        let ReadIntoRequest::Promise(deferred) = self;
        deferred.resolve(global, iter_result(global, chunk, false));
        Ok(())
    }

    pub fn close_steps(self, global: &JSGlobalObject, chunk: JSValue) -> JsResult<()> {
        let ReadIntoRequest::Promise(deferred) = self;
        deferred.resolve(global, iter_result(global, chunk, true));
        Ok(())
    }

    pub fn error_steps(self, global: &JSGlobalObject, reason: JSValue) -> JsResult<()> {
        let ReadIntoRequest::Promise(deferred) = self;
        deferred.reject(global, reason);
        Ok(())
    }
}

#[derive(Trace, Finalize)]
pub struct ReadableStreamBYOBReader {
    pub(crate) stream: JSValue,
    pub(crate) closed: Deferred,
    pub(crate) read_into_requests: VecDeque<ReadIntoRequest>,
}

pub fn is_byob_reader(value: &JSValue) -> bool {
    value.as_class_ref::<ReadableStreamBYOBReader>().is_some()
}

pub fn byob_reader_of(value: &JSValue) -> ClassRef<ReadableStreamBYOBReader> {
    value.as_class_ref::<ReadableStreamBYOBReader>().expect("a ReadableStreamBYOBReader")
}

fn reader_of_stream(global: &JSGlobalObject, stream: &JSValue) -> JsResult<JSValue> {
    Ok(stream_of(stream).borrow(global)?.reader.clone())
}

/// `ReadableStreamGetNumReadIntoRequests`.
pub fn num_read_into_requests(global: &JSGlobalObject, stream: &JSValue) -> JsResult<usize> {
    let reader = reader_of_stream(global, stream)?;
    match reader.as_class_ref::<ReadableStreamBYOBReader>() {
        Some(reader) => Ok(reader.borrow(global)?.read_into_requests.len()),
        None => Ok(0),
    }
}

/// `ReadableStreamAddReadIntoRequest`.
pub fn add_read_into_request(global: &JSGlobalObject, stream: &JSValue, request: ReadIntoRequest) -> JsResult<()> {
    let reader = reader_of_stream(global, stream)?;
    byob_reader_of(&reader).borrow_mut(global)?.read_into_requests.push_back(request);
    Ok(())
}

/// `ReadableStreamFulfillReadIntoRequest`.
pub fn fulfill_read_into_request(global: &JSGlobalObject, stream: &JSValue, chunk: JSValue, done: bool) -> JsResult<()> {
    let reader = reader_of_stream(global, stream)?;
    let request = byob_reader_of(&reader).borrow_mut(global)?.read_into_requests.pop_front();
    match request {
        Some(request) if done => request.close_steps(global, chunk),
        Some(request) => request.chunk_steps(global, chunk),
        None => Ok(()),
    }
}

/// `ReadableStreamBYOBReaderErrorReadIntoRequests`.
pub fn error_read_into_requests(global: &JSGlobalObject, reader: &JSValue, reason: JSValue) -> JsResult<()> {
    let requests = std::mem::take(&mut byob_reader_of(reader).borrow_mut(global)?.read_into_requests);
    for request in requests {
        request.error_steps(global, reason.clone())?;
    }
    Ok(())
}

/// What `ReadableStreamCancel` does with a BYOB reader's requests: each ends with no view.
pub fn close_read_into_requests(global: &JSGlobalObject, reader: &JSValue) -> JsResult<()> {
    let requests = std::mem::take(&mut byob_reader_of(reader).borrow_mut(global)?.read_into_requests);
    for request in requests {
        request.close_steps(global, JSValue::UNDEFINED)?;
    }
    Ok(())
}

/// `SetUpReadableStreamBYOBReader`.
pub fn set_up(global: &JSGlobalObject, reader: &JSValue, stream: &JSValue) -> JsResult<()> {
    if readable_stream::is_locked(global, stream)? {
        return Err(global.throw_value(invalid_state(global, "ReadableStream is locked")));
    }
    let controller = stream_of(stream).borrow(global)?.controller.clone();
    if !byte_controller::is_byte_controller(&controller) {
        return Err(global.throw_type_error(format_args!(
            "A BYOB reader requires a ReadableStream with an underlying byte source"
        )));
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
    let r = byob_reader_of(reader);
    let mut r = r.borrow_mut(global)?;
    r.stream = stream.clone();
    r.closed = closed;
    r.read_into_requests.clear();
    Ok(())
}

/// `AcquireReadableStreamBYOBReader`.
pub fn acquire(global: &JSGlobalObject, stream: &JSValue) -> JsResult<JSValue> {
    let reader = bun_js::class::to_js(global, ReadableStreamBYOBReader::detached(global))?;
    set_up(global, &reader, stream)?;
    Ok(reader)
}

impl ReadableStreamBYOBReader {
    fn detached(global: &JSGlobalObject) -> ReadableStreamBYOBReader {
        ReadableStreamBYOBReader {
            stream: JSValue::UNDEFINED,
            closed: Deferred::new(global),
            read_into_requests: VecDeque::new(),
        }
    }
}

/// `ReadableStreamBYOBReaderRelease`.
pub fn release(global: &JSGlobalObject, reader: &JSValue) -> JsResult<()> {
    let (stream, closed) = {
        let r = byob_reader_of(reader);
        let r = r.borrow(global)?;
        (r.stream.clone(), r.closed.clone())
    };
    let released = invalid_state(global, "Reader released");
    if readable_stream::state(global, &stream)? == State::Readable {
        closed.reject(global, released);
        mark_handled(global, &closed.promise);
    } else {
        byob_reader_of(reader).borrow_mut(global)?.closed = Deferred::rejected_handled(global, released);
    }
    let controller = stream_of(&stream).borrow(global)?.controller.clone();
    byte_controller::release_steps(global, &controller)?;
    stream_of(&stream).borrow_mut(global)?.reader = JSValue::UNDEFINED;
    byob_reader_of(reader).borrow_mut(global)?.stream = JSValue::UNDEFINED;
    error_read_into_requests(global, reader, invalid_state(global, "Releasing reader"))
}

fn read(global: &JSGlobalObject, reader: &JSValue, frame: &CallFrame<'_>) -> JsResult<JSValue> {
    let [view, options] = frame.arguments_as_array::<2>();
    let reject = |message: &str, range: bool| {
        let error = match range {
            true => global.create_range_error_instance(format_args!("{message}")),
            false => internals::type_error(global, message),
        };
        Ok(internals::rejected(global, error))
    };
    let Some(info) = crate::engine::view_info(global, &view)? else {
        return reject("ReadableStreamBYOBReader.read() expects an ArrayBufferView", false);
    };
    if info.byte_length == 0 {
        return reject("view must have a non-zero byteLength", false);
    }
    if crate::engine::array_buffer_byte_length(&info.buffer) == 0 {
        return reject("view's buffer must have a non-zero byteLength", false);
    }
    if crate::engine::is_detached(&info.buffer) {
        return reject("view's buffer has been detached", false);
    }
    if !options.is_undefined_or_null() && !options.is_object() {
        return reject("options must be an object", false);
    }
    let min = match member(global, &options, "min")? {
        min if min.is_undefined() => 1,
        min => min.to_number(global)? as usize,
    };
    if min == 0 {
        return reject("options.min must be greater than 0", false);
    }
    if min > info.length {
        return reject("options.min must be less than or equal to view's length", true);
    }
    let stream = byob_reader_of(reader).borrow(global)?.stream.clone();
    if stream.is_undefined() {
        return Ok(internals::rejected(global, invalid_state(global, "The reader is not attached to a stream")));
    }
    let deferred = Deferred::new(global);
    let request = ReadIntoRequest::Promise(deferred.clone());
    // ReadableStreamBYOBReaderRead.
    let (state, stored_error, controller) = {
        let s = stream_of(&stream);
        let mut s = s.borrow_mut(global)?;
        s.disturbed = true;
        (s.state, s.stored_error.clone(), s.controller.clone())
    };
    if state == State::Errored {
        request.error_steps(global, stored_error)?;
    } else {
        byte_controller::pull_into(global, &controller, &view, min, request)?;
    }
    Ok(deferred.promise)
}

impl JsClass for ReadableStreamBYOBReader {
    const NAME: &'static str = "ReadableStreamBYOBReader";
    const LENGTH: usize = 1;

    fn construct(global: &JSGlobalObject, frame: &CallFrame<'_>) -> JsResult<Self> {
        if frame.argument(0).as_class_ref::<readable_stream::ReadableStream>().is_none() {
            return Err(global.throw_type_error(format_args!(
                "ReadableStreamBYOBReader constructor requires a ReadableStream as its first argument"
            )));
        }
        Ok(ReadableStreamBYOBReader::detached(global))
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
        class.method("read", 1, attributes, |this, global, frame| read(global, &this.to_js(), frame))?;
        class.method("releaseLock", 0, attributes, |this, global, _| {
            if !this.borrow(global)?.stream.is_undefined() {
                release(global, &this.to_js())?;
            }
            Ok(JSValue::UNDEFINED)
        })?;
        Ok(())
    }
}
