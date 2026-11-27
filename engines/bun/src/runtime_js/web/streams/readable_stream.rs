//! `ReadableStream` (Streams §4.2, §4.9.1, §4.9.3): JSReadableStream.cpp and ReadableStreamOperations.cpp, with Bun's
//! `text()`, `bytes()`, `json()` and `blob()` of BunStreamConsumers.cpp.

use boa_gc::{Finalize, Trace};
use bun_js::class::{Attributes, ClassBuilder, ClassRef};
use bun_js::{CallFrame, ErrorCode, JSArray, JSGlobalObject, JSValue, JsClass, JsResult};

use super::internals::{self, Deferred, Reaction, callback_member, invalid_state, member, upon};
use super::readable_stream_default_controller::{self as controller, SourceAlgorithms};
use super::readable_stream_default_reader::{self as reader, ReadRequest, reader_of};
use super::readable_byte_stream_controller as byte_controller;
use super::readable_stream_byob_reader as byob_reader;
use super::{readable_stream_async_iterator, stream_pipe_to_operation, stream_tee_state};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum State {
    Readable,
    Closed,
    Errored,
}

#[derive(Trace, Finalize)]
pub struct ReadableStream {
    #[unsafe_ignore_trace]
    pub(crate) state: State,
    pub(crate) reader: JSValue,
    pub(crate) controller: JSValue,
    pub(crate) stored_error: JSValue,
    pub(crate) disturbed: bool,
}

impl ReadableStream {
    /// `InitializeReadableStream`.
    fn initial() -> ReadableStream {
        ReadableStream {
            state: State::Readable,
            reader: JSValue::UNDEFINED,
            controller: JSValue::UNDEFINED,
            stored_error: JSValue::UNDEFINED,
            disturbed: false,
        }
    }
}

pub fn stream_of(value: &JSValue) -> ClassRef<ReadableStream> {
    value.as_class_ref::<ReadableStream>().expect("a ReadableStream")
}

pub fn state(global: &JSGlobalObject, stream: &JSValue) -> JsResult<State> {
    Ok(stream_of(stream).borrow(global)?.state)
}

/// `IsReadableStreamLocked`.
pub fn is_locked(global: &JSGlobalObject, stream: &JSValue) -> JsResult<bool> {
    Ok(!stream_of(stream).borrow(global)?.reader.is_undefined())
}

/// `ReadableStreamGetNumReadRequests`.
pub fn num_read_requests(global: &JSGlobalObject, stream: &JSValue) -> JsResult<usize> {
    let reader = stream_of(stream).borrow(global)?.reader.clone();
    match reader.as_class_ref::<reader::ReadableStreamDefaultReader>() {
        Some(reader) => Ok(reader.borrow(global)?.read_requests.len()),
        None => Ok(0),
    }
}

/// `ReadableStreamAddReadRequest`.
pub fn add_read_request(global: &JSGlobalObject, stream: &JSValue, request: ReadRequest) -> JsResult<()> {
    let reader = stream_of(stream).borrow(global)?.reader.clone();
    reader_of(&reader).borrow_mut(global)?.read_requests.push_back(request);
    Ok(())
}

/// `ReadableStreamFulfillReadRequest`.
pub fn fulfill_read_request(global: &JSGlobalObject, stream: &JSValue, chunk: JSValue, done: bool) -> JsResult<()> {
    let reader = stream_of(stream).borrow(global)?.reader.clone();
    let request = reader_of(&reader).borrow_mut(global)?.read_requests.pop_front();
    match request {
        Some(request) if done => request.close_steps(global),
        Some(request) => request.chunk_steps(global, chunk),
        None => Ok(()),
    }
}

/// `ReadableStreamCancel`.
pub fn cancel(global: &JSGlobalObject, stream: &JSValue, reason: JSValue) -> JsResult<JSValue> {
    let (state, stored_error, controller) = {
        let s = stream_of(stream);
        let mut s = s.borrow_mut(global)?;
        s.disturbed = true;
        (s.state, s.stored_error.clone(), s.controller.clone())
    };
    match state {
        State::Closed => return Ok(internals::resolved(global, JSValue::UNDEFINED)),
        State::Errored => return Ok(internals::rejected(global, stored_error)),
        State::Readable => {}
    }
    close(global, stream)?;
    let reader = stream_of(stream).borrow(global)?.reader.clone();
    if byob_reader::is_byob_reader(&reader) {
        byob_reader::close_read_into_requests(global, &reader)?;
    }
    let source_cancel = match byte_controller::is_byte_controller(&controller) {
        true => byte_controller::cancel_steps(global, &controller, reason)?,
        false => controller::cancel_steps(global, &controller, reason)?,
    };
    let to_undefined: Reaction<internals::Nothing> = |_, _, _| Ok(JSValue::UNDEFINED);
    Ok(internals::react(global, &source_cancel, internals::Nothing, Some(to_undefined), None))
}

/// `ReadableStreamClose`.
pub fn close(global: &JSGlobalObject, stream: &JSValue) -> JsResult<()> {
    let reader = {
        let s = stream_of(stream);
        let mut s = s.borrow_mut(global)?;
        s.state = State::Closed;
        s.reader.clone()
    };
    if reader.is_undefined() {
        return Ok(());
    }
    if byob_reader::is_byob_reader(&reader) {
        byob_reader::byob_reader_of(&reader).borrow(global)?.closed.resolve(global, JSValue::UNDEFINED);
        return Ok(());
    }
    let (closed, requests) = {
        let r = reader_of(&reader);
        let mut r = r.borrow_mut(global)?;
        (r.closed.clone(), std::mem::take(&mut r.read_requests))
    };
    closed.resolve(global, JSValue::UNDEFINED);
    for request in requests {
        request.close_steps(global)?;
    }
    Ok(())
}

/// `ReadableStreamError`.
pub fn error(global: &JSGlobalObject, stream: &JSValue, reason: JSValue) -> JsResult<()> {
    let reader = {
        let s = stream_of(stream);
        let mut s = s.borrow_mut(global)?;
        s.state = State::Errored;
        s.stored_error = reason.clone();
        s.reader.clone()
    };
    if reader.is_undefined() {
        return Ok(());
    }
    if byob_reader::is_byob_reader(&reader) {
        let closed = byob_reader::byob_reader_of(&reader).borrow(global)?.closed.clone();
        closed.reject(global, reason.clone());
        internals::mark_handled(global, &closed.promise);
        return byob_reader::error_read_into_requests(global, &reader, reason);
    }
    let closed = reader_of(&reader).borrow(global)?.closed.clone();
    closed.reject(global, reason.clone());
    internals::mark_handled(global, &closed.promise);
    reader::error_read_requests(global, &reader, reason)
}

/// `CreateReadableStream` with a Rust source: a stream of high water mark `high_water_mark` and chunks of size 1.
pub fn create(global: &JSGlobalObject, algorithms: SourceAlgorithms, high_water_mark: f64) -> JsResult<JSValue> {
    create_with(global, JSValue::UNDEFINED, algorithms, high_water_mark, JSValue::UNDEFINED)
}

/// `CreateReadableStream` whose start algorithm returns `start`, with the strategy size `size`.
pub fn create_with(
    global: &JSGlobalObject,
    start: JSValue,
    algorithms: SourceAlgorithms,
    high_water_mark: f64,
    size: JSValue,
) -> JsResult<JSValue> {
    let stream = bun_js::class::to_js(global, ReadableStream::initial())?;
    controller::set_up(global, &stream, move |_| Ok(start), algorithms, high_water_mark, size)?;
    Ok(stream)
}

/// A stream of `chunks` and then its end, as Blob.stream() makes for bytes in memory.
pub fn of_chunks(global: &JSGlobalObject, chunks: &[JSValue]) -> JsResult<JSValue> {
    let stream = create(global, SourceAlgorithms::None, 0.0)?;
    let controller = stream_of(&stream).borrow(global)?.controller.clone();
    for chunk in chunks {
        controller::enqueue(global, &controller, chunk.clone())?;
    }
    controller::close(global, &controller)?;
    Ok(stream)
}

/// `SetUpReadableStreamDefaultControllerFromUnderlyingSource`, after the dictionary is converted.
fn set_up_from_underlying_source(
    global: &JSGlobalObject,
    stream: &JSValue,
    source: &JSValue,
    start: JSValue,
    pull: JSValue,
    cancel: JSValue,
    high_water_mark: f64,
    size: JSValue,
) -> JsResult<()> {
    let algorithms = SourceAlgorithms::Js {
        source: source.clone(),
        pull,
        cancel,
    };
    let source = source.clone();
    controller::set_up(
        global,
        stream,
        move |controller| {
            if start.is_callable() {
                start.call(global, source, std::slice::from_ref(controller))
            } else {
                Ok(JSValue::UNDEFINED)
            }
        },
        algorithms,
        high_water_mark,
        size,
    )?;
    Ok(())
}

/// The ReadableStream constructor's steps, on the new stream.
fn construct_steps(global: &JSGlobalObject, stream: &JSValue, frame: &CallFrame<'_>) -> JsResult<()> {
    let [underlying_source, strategy] = frame.arguments_as_array::<2>();
    if !underlying_source.is_undefined_or_null() && !underlying_source.is_object() {
        return Err(global.throw_type_error(format_args!(
            "ReadableStream constructor takes an object as first argument"
        )));
    }
    if !strategy.is_undefined() && !strategy.is_object() {
        return Err(global.throw_type_error(format_args!(
            "ReadableStream constructor takes an object as second argument, if any"
        )));
    }
    // The UnderlyingSource dictionary, its members in order.
    let what = "The underlying source";
    let auto_allocate_chunk_size = member(global, &underlying_source, "autoAllocateChunkSize")?;
    let cancel = callback_member(global, &underlying_source, "cancel", what)?;
    let pull = callback_member(global, &underlying_source, "pull", what)?;
    let start = callback_member(global, &underlying_source, "start", what)?;
    let kind = member(global, &underlying_source, "type")?;
    let kind = match kind.is_undefined() {
        true => None,
        false => Some(kind.to_utf8(global)?),
    };
    let strategy = internals::strategy(
        global,
        &strategy,
        "ReadableStream constructor takes an object as second argument, if any",
    )?;
    match kind.as_deref() {
        None => {}
        Some("bytes") => {
            if !strategy.size.is_undefined() {
                return Err(global.throw_range_error(format_args!(
                    "The queuing strategy of a readable byte stream cannot have a size function"
                )));
            }
            let auto_allocate_chunk_size = match auto_allocate_chunk_size.is_undefined() {
                true => None,
                false => {
                    let size = auto_allocate_chunk_size.to_number(global)?;
                    if !(size >= 1.0) || size.is_infinite() {
                        return Err(global.throw_type_error(format_args!(
                            "autoAllocateChunkSize must be greater than 0"
                        )));
                    }
                    Some(size as usize)
                }
            };
            let high_water_mark = internals::extract_high_water_mark(global, &strategy, 0.0)?;
            return byte_controller::set_up_from_underlying_source(
                global,
                stream,
                &underlying_source,
                start,
                pull,
                cancel,
                high_water_mark,
                auto_allocate_chunk_size,
            );
        }
        Some("direct") => {
            let close = callback_member(global, &underlying_source, "close", what)?;
            let high_water_mark = internals::extract_high_water_mark(global, &strategy, 0.0)?;
            let direct = super::direct_stream_controller::create(global, stream, &underlying_source, pull, cancel, close)?;
            controller::set_up(
                global,
                stream,
                |_| Ok(JSValue::UNDEFINED),
                SourceAlgorithms::Direct { controller: direct },
                high_water_mark,
                JSValue::UNDEFINED,
            )?;
            return Ok(());
        }
        Some(other) => {
            return Err(global.throw_type_error(format_args!(
                "'{other}' is not a valid underlying source 'type'; expected \"bytes\", \"direct\", or undefined"
            )));
        }
    }
    let high_water_mark = internals::extract_high_water_mark(global, &strategy, 1.0)?;
    set_up_from_underlying_source(
        global,
        stream,
        &underlying_source,
        start,
        pull,
        cancel,
        high_water_mark,
        strategy.size,
    )
}

/// `ReadableStream.from(asyncIterable)` (Streams §4.9.1 `ReadableStreamFromIterable`).
fn from(global: &JSGlobalObject, frame: &CallFrame<'_>) -> JsResult<JSValue> {
    let iterable = frame.argument(0);
    let not_iterable = |global: &JSGlobalObject| -> JsResult<bun_js::JsError> {
        let shown = match iterable.is_string() {
            true => format!("\"{}\"", iterable.to_utf8(global)?),
            false => iterable.to_utf8(global).unwrap_or_else(|_| "value".to_owned()),
        };
        Ok(ErrorCode::ARG_NOT_ITERABLE.throw(global, format_args!("{shown} must be iterable")))
    };
    let (method, is_async) = match crate::engine::symbol_method(global, &iterable, "asyncIterator")? {
        Some(method) => (method, true),
        None => match crate::engine::symbol_method(global, &iterable, "iterator")? {
            Some(method) => (method, false),
            None => return Err(not_iterable(global)?),
        },
    };
    let iterator = method.call(global, iterable.clone(), &[])?;
    if !iterator.is_object() {
        return Err(global.throw_type_error(format_args!("Invalid state: The iterator method must return an object")));
    }
    let next = iterator.get(global, "next")?.unwrap_or_default();
    create(global, SourceAlgorithms::Iterator { iterator, next, is_async }, 0.0)
}

#[derive(Clone, Trace, Finalize)]
#[boa_gc(unsafe_no_drop)]
struct IteratorStep {
    controller: JSValue,
    is_async: bool,
}

/// The pull algorithm of `ReadableStream.from()`: the iterator's next value is enqueued, its end closes the stream.
pub fn iterator_pull(
    global: &JSGlobalObject,
    controller: &JSValue,
    iterator: &JSValue,
    next: &JSValue,
    is_async: bool,
) -> JsResult<JSValue> {
    let result = match next.call(global, iterator.clone(), &[]) {
        Ok(result) => result,
        Err(thrown) => return Ok(internals::rejected(global, internals::catch(global, thrown)?)),
    };
    let result = internals::resolved(global, result);
    let step: Reaction<IteratorStep> = |global, step, result| {
        if !result.is_object() {
            return Err(global.throw_type_error(format_args!(
                "The promise returned by the async iterator's next() method must fulfill with an object"
            )));
        }
        let done = result.get(global, "done")?.is_some_and(|done| done.to_boolean());
        if done {
            controller::close(global, &step.controller)?;
            return Ok(JSValue::UNDEFINED);
        }
        let value = result.get(global, "value")?.unwrap_or_default();
        if step.is_async {
            controller::enqueue(global, &step.controller, value)?;
            return Ok(JSValue::UNDEFINED);
        }
        // A sync iterator's value is awaited, as CreateAsyncFromSyncIterator does.
        let enqueue: Reaction<IteratorStep> = |global, step, value| {
            controller::enqueue(global, &step.controller, value)?;
            Ok(JSValue::UNDEFINED)
        };
        Ok(internals::react(global, &internals::resolved(global, value), step.clone(), Some(enqueue), None))
    };
    let captures = IteratorStep {
        controller: controller.clone(),
        is_async,
    };
    Ok(internals::react(global, &result, captures, Some(step), None))
}

/// The cancel algorithm of `ReadableStream.from()`: the iterator's `return(reason)`, if it has one.
pub fn iterator_cancel(global: &JSGlobalObject, iterator: &JSValue, reason: JSValue) -> JsResult<JSValue> {
    internals::promise_of(global, || {
        let Some(method) = iterator.get(global, "return")? else {
            return Ok(JSValue::UNDEFINED);
        };
        if !method.is_callable() {
            return Err(global.throw_type_error(format_args!("The async iterator's return property must be callable")));
        }
        let result = method.call(global, iterator.clone(), &[reason])?;
        let check: Reaction<internals::Nothing> = |global, _, result| {
            if !result.is_object() {
                return Err(global.throw_type_error(format_args!(
                    "The promise returned by the async iterator's return() method must fulfill with an object"
                )));
            }
            Ok(JSValue::UNDEFINED)
        };
        Ok(internals::react(global, &internals::resolved(global, result), internals::Nothing, Some(check), None))
    })
}

/// What a consumer (`text()`, `bytes()`, ...) makes of the bytes.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Consume {
    Text,
    Bytes,
    ArrayBuffer,
    Json,
    Blob,
    /// The chunks themselves, as `Bun.readableStreamToArray()` gives them.
    Array,
}

#[derive(Trace, Finalize)]
pub struct StreamConsumer {
    reader: JSValue,
    #[unsafe_ignore_trace]
    kind: Consume,
    bytes: Vec<u8>,
    chunks: Vec<JSValue>,
    deferred: Deferred,
}

impl JsClass for StreamConsumer {
    const NAME: &'static str = "StreamConsumer";

    fn define(_: &mut ClassBuilder<'_, Self>) -> JsResult<()> {
        Ok(())
    }
}

/// Reads the whole stream, as BunStreamConsumers.cpp does for `stream.text()` and a body's readers.
pub fn consume(global: &JSGlobalObject, stream: &JSValue, kind: Consume) -> JsResult<JSValue> {
    if stream_of(stream).borrow(global)?.disturbed {
        let error = invalid_state(global, "ReadableStream has already been used");
        return Ok(internals::rejected(global, error));
    }
    if is_locked(global, stream)? {
        return Ok(internals::rejected(global, invalid_state(global, "ReadableStream is locked")));
    }
    let stream_controller = stream_of(stream).borrow(global)?.controller.clone();
    if let Some(direct) = controller::direct_controller(global, &stream_controller)? {
        super::direct_stream_controller::mark_one_shot(global, &direct)?;
    }
    let reader = reader::acquire(global, stream)?;
    let deferred = Deferred::new(global);
    let consumer = bun_js::class::to_js(
        global,
        StreamConsumer {
            reader: reader.clone(),
            kind,
            bytes: Vec::new(),
            chunks: Vec::new(),
            deferred: deferred.clone(),
        },
    )?;
    reader::read(global, &reader, ReadRequest::Consume(consumer))?;
    Ok(deferred.promise)
}

fn consumer_of(value: &JSValue) -> ClassRef<StreamConsumer> {
    value.as_class_ref::<StreamConsumer>().expect("a StreamConsumer")
}

pub(crate) fn consume_chunk_steps(global: &JSGlobalObject, consumer: &JSValue, chunk: JSValue) -> JsResult<()> {
    let kind = consumer_of(consumer).borrow(global)?.kind;
    let bytes = match crate::engine::buffer_source_bytes(global, &chunk) {
        _ if kind == Consume::Array => {
            consumer_of(consumer).borrow_mut(global)?.chunks.push(chunk);
            Vec::new()
        }
        Some(bytes) => bytes,
        None if chunk.is_string() => chunk.to_utf8(global)?.into_bytes(),
        None => {
            let (reader, deferred) = {
                let c = consumer_of(consumer);
                let c = c.borrow(global)?;
                (c.reader.clone(), c.deferred.clone())
            };
            let error = internals::type_error(global, "ReadableStream chunk is not a string, an ArrayBuffer or an ArrayBufferView");
            let _ = cancel(global, &reader_of(&reader).borrow(global)?.stream.clone(), error.clone())?;
            reader::release(global, &reader)?;
            deferred.reject(global, error);
            return Ok(());
        }
    };
    consumer_of(consumer).borrow_mut(global)?.bytes.extend_from_slice(&bytes);
    // The next read waits for a microtask, so that a queue of many chunks does not nest a read in each.
    let read_next: Reaction<JSValue> = |global, consumer, _| {
        let reader = consumer_of(consumer).borrow(global)?.reader.clone();
        reader::read(global, &reader, ReadRequest::Consume(consumer.clone()))?;
        Ok(JSValue::UNDEFINED)
    };
    let tick = internals::resolved(global, JSValue::UNDEFINED);
    upon(global, &tick, consumer.clone(), Some(read_next), None);
    Ok(())
}

pub(crate) fn consume_close_steps(global: &JSGlobalObject, consumer: &JSValue) -> JsResult<()> {
    let (reader, kind, bytes, chunks, deferred) = {
        let c = consumer_of(consumer);
        let mut c = c.borrow_mut(global)?;
        (
            c.reader.clone(),
            c.kind,
            std::mem::take(&mut c.bytes),
            std::mem::take(&mut c.chunks),
            c.deferred.clone(),
        )
    };
    if !reader_of(&reader).borrow(global)?.stream.is_undefined() {
        reader::release(global, &reader)?;
    }
    let result = internals::promise_of(global, || match kind {
        Consume::Text => Ok(crate::engine::decode_utf8_without_bom(&bytes)),
        Consume::Bytes => crate::engine::uint8_array(global, &bytes),
        Consume::ArrayBuffer => crate::engine::array_buffer(global, &bytes),
        Consume::Json => crate::engine::parse_json(global, crate::engine::decode_utf8_without_bom(&bytes)),
        Consume::Blob => crate::webcore::blob::Blob::init(bytes).to_js(global),
        Consume::Array => Ok(JSArray::create(global, &chunks).to_js()),
    })?;
    deferred.resolve(global, result);
    Ok(())
}

pub(crate) fn consume_error_steps(global: &JSGlobalObject, consumer: &JSValue, reason: JSValue) -> JsResult<()> {
    let deferred = consumer_of(consumer).borrow(global)?.deferred.clone();
    deferred.reject(global, reason);
    Ok(())
}

fn get_reader(this: &ClassRef<ReadableStream>, global: &JSGlobalObject, frame: &CallFrame<'_>) -> JsResult<JSValue> {
    let options = frame.argument(0);
    if !options.is_undefined_or_null() && !options.is_object() {
        return Err(global.throw_type_error(format_args!("ReadableStream.getReader takes an object as first argument, if any")));
    }
    let mode = member(global, &options, "mode")?;
    if mode.is_undefined() {
        return reader::acquire(global, &this.to_js());
    }
    let mode = mode.to_utf8(global)?;
    if mode == "byob" {
        return byob_reader::acquire(global, &this.to_js());
    }
    Err(ErrorCode::INVALID_ARG_VALUE.throw(
        global,
        format_args!("The property 'options.mode' is invalid. Received '{mode}'"),
    ))
}

fn tee(this: &ClassRef<ReadableStream>, global: &JSGlobalObject, _: &CallFrame<'_>) -> JsResult<JSValue> {
    let [branch1, branch2] = stream_tee_state::default_tee(global, &this.to_js())?;
    Ok(JSArray::create(global, &[branch1, branch2]).to_js())
}

fn pipe_options(global: &JSGlobalObject, options: &JSValue) -> JsResult<stream_pipe_to_operation::Options> {
    if !options.is_undefined_or_null() && !options.is_object() {
        return Err(global.throw_type_error(format_args!("The pipe options must be an object")));
    }
    // StreamPipeOptions, its members in order.
    let prevent_abort = member(global, options, "preventAbort")?.to_boolean();
    let prevent_cancel = member(global, options, "preventCancel")?.to_boolean();
    let prevent_close = member(global, options, "preventClose")?.to_boolean();
    let signal = member(global, options, "signal")?;
    if !signal.is_undefined() && !crate::engine::is_instance_of_global(global, &signal, "AbortSignal")? {
        return Err(global.throw_type_error(format_args!("The pipe options' 'signal' property must be an AbortSignal")));
    }
    Ok(stream_pipe_to_operation::Options {
        prevent_abort,
        prevent_cancel,
        prevent_close,
        signal,
    })
}

fn pipe_to(this: &ClassRef<ReadableStream>, global: &JSGlobalObject, frame: &CallFrame<'_>) -> JsResult<JSValue> {
    let [destination, options] = frame.arguments_as_array::<2>();
    let source = this.to_js();
    internals::promise_of(global, || {
        if !super::writable_stream::is_writable_stream(&destination) {
            return Err(global.throw_type_error(format_args!(
                "ReadableStream.prototype.pipeTo requires a WritableStream destination"
            )));
        }
        let options = pipe_options(global, &options)?;
        if is_locked(global, &source)? {
            return Err(global.throw_value(invalid_state(global, "The ReadableStream is locked")));
        }
        if super::writable_stream::is_locked(global, &destination)? {
            return Err(global.throw_value(invalid_state(global, "The WritableStream is locked")));
        }
        stream_pipe_to_operation::pipe_to(global, &source, &destination, options)
    })
}

fn pipe_through(this: &ClassRef<ReadableStream>, global: &JSGlobalObject, frame: &CallFrame<'_>) -> JsResult<JSValue> {
    let [transform, options] = frame.arguments_as_array::<2>();
    if !transform.is_object() {
        return Err(global.throw_type_error(format_args!(
            "pipeThrough() expects an object with 'readable' and 'writable' properties"
        )));
    }
    let readable = member(global, &transform, "readable")?;
    if readable.as_class_ref::<ReadableStream>().is_none() {
        return Err(global.throw_type_error(format_args!("The transform's 'readable' property must be a ReadableStream")));
    }
    let writable = member(global, &transform, "writable")?;
    if !super::writable_stream::is_writable_stream(&writable) {
        return Err(global.throw_type_error(format_args!("The transform's 'writable' property must be a WritableStream")));
    }
    let options = pipe_options(global, &options)?;
    let source = this.to_js();
    if is_locked(global, &source)? {
        return Err(global.throw_value(invalid_state(global, "The ReadableStream is locked")));
    }
    if super::writable_stream::is_locked(global, &writable)? {
        return Err(global.throw_value(invalid_state(global, "The WritableStream is locked")));
    }
    let promise = stream_pipe_to_operation::pipe_to(global, &source, &writable, options)?;
    internals::mark_handled(global, &promise);
    Ok(readable)
}

fn values(this: &ClassRef<ReadableStream>, global: &JSGlobalObject, frame: &CallFrame<'_>) -> JsResult<JSValue> {
    let options = frame.argument(0);
    if !options.is_undefined_or_null() && !options.is_object() {
        return Err(global.throw_type_error(format_args!("values() options must be an object")));
    }
    let prevent_cancel = member(global, &options, "preventCancel")?.to_boolean();
    readable_stream_async_iterator::create(global, &this.to_js(), prevent_cancel)
}

fn consumer_method(kind: Consume) -> bun_js::class::Method<ReadableStream> {
    match kind {
        Consume::Text => |this, global, _| consume(global, &this.to_js(), Consume::Text),
        Consume::Bytes => |this, global, _| consume(global, &this.to_js(), Consume::Bytes),
        Consume::ArrayBuffer => |this, global, _| consume(global, &this.to_js(), Consume::ArrayBuffer),
        Consume::Json => |this, global, _| consume(global, &this.to_js(), Consume::Json),
        Consume::Blob => |this, global, _| consume(global, &this.to_js(), Consume::Blob),
        Consume::Array => |this, global, _| consume(global, &this.to_js(), Consume::Array),
    }
}

fn to_consumer(kind: Consume) -> bun_js::JSHostFn {
    fn stream_argument(global: &JSGlobalObject, frame: &CallFrame<'_>) -> JsResult<JSValue> {
        let stream = frame.argument(0);
        if stream.as_class_ref::<ReadableStream>().is_none() {
            return Err(ErrorCode::INVALID_ARG_TYPE.throw(global, format_args!("Expected ReadableStream")));
        }
        Ok(stream)
    }
    match kind {
        Consume::Text => |global, frame| consume(global, &stream_argument(global, frame)?, Consume::Text),
        Consume::Bytes => |global, frame| consume(global, &stream_argument(global, frame)?, Consume::Bytes),
        Consume::ArrayBuffer => |global, frame| consume(global, &stream_argument(global, frame)?, Consume::ArrayBuffer),
        Consume::Json => |global, frame| consume(global, &stream_argument(global, frame)?, Consume::Json),
        Consume::Blob => |global, frame| consume(global, &stream_argument(global, frame)?, Consume::Blob),
        Consume::Array => |global, frame| consume(global, &stream_argument(global, frame)?, Consume::Array),
    }
}

/// `Bun.readableStreamToText()` and the others of its kind.
pub fn bun_functions(global: &JSGlobalObject) -> Vec<(&'static str, JSValue)> {
    [
        ("readableStreamToText", Consume::Text),
        ("readableStreamToBytes", Consume::Bytes),
        ("readableStreamToArrayBuffer", Consume::ArrayBuffer),
        ("readableStreamToJSON", Consume::Json),
        ("readableStreamToBlob", Consume::Blob),
        ("readableStreamToArray", Consume::Array),
    ]
    .into_iter()
    .map(|(name, kind)| (name, bun_js::JSFunction::create(global, name, to_consumer(kind), 1)))
    .collect()
}

impl JsClass for ReadableStream {
    const NAME: &'static str = "ReadableStream";

    fn construct(_: &JSGlobalObject, _: &CallFrame<'_>) -> JsResult<Self> {
        Ok(ReadableStream::initial())
    }

    fn initialize(this: &ClassRef<Self>, global: &JSGlobalObject, frame: &CallFrame<'_>) -> JsResult<()> {
        construct_steps(global, &this.to_js(), frame)
    }

    fn define(class: &mut ClassBuilder<'_, Self>) -> JsResult<()> {
        let attributes = Attributes::DEFAULT;
        class.getter("locked", attributes, |this, global| {
            Ok(JSValue::js_boolean(!this.borrow(global)?.reader.is_undefined()))
        })?;
        class.method("cancel", 1, attributes, |this, global, frame| {
            let stream = this.to_js();
            if is_locked(global, &stream)? {
                return Ok(internals::rejected(global, invalid_state(global, "ReadableStream is locked")));
            }
            cancel(global, &stream, frame.argument(0))
        })?;
        class.method("getReader", 0, attributes, get_reader)?;
        class.method("pipeThrough", 1, attributes, pipe_through)?;
        class.method("pipeTo", 1, attributes, pipe_to)?;
        class.method("tee", 0, attributes, tee)?;
        class.method("values", 0, attributes, values)?;
        class.method("blob", 0, attributes, consumer_method(Consume::Blob))?;
        class.method("bytes", 0, attributes, consumer_method(Consume::Bytes))?;
        class.method("json", 0, attributes, consumer_method(Consume::Json))?;
        class.method("text", 0, attributes, consumer_method(Consume::Text))?;
        class.static_method("from", 1, attributes, from)?;
        Ok(())
    }
}

/// `ReadableStream`, with `[Symbol.asyncIterator]` that is `values`.
pub fn constructor(global: &JSGlobalObject) -> JsResult<JSValue> {
    let constructor = bun_js::class::constructor::<ReadableStream>(global)?;
    let prototype = constructor.get(global, "prototype")?.unwrap_or_default();
    let values = prototype.get(global, "values")?.unwrap_or_default();
    crate::engine::define_async_iterator(global, &prototype, values)?;
    Ok(constructor)
}
