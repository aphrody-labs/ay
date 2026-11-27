//! `ReadableByteStreamController` and `ReadableStreamBYOBRequest` (Streams §4.7, §4.8, §4.9.6):
//! JSReadableByteStreamController.cpp, JSReadableStreamBYOBRequest.cpp and JSPullIntoDescriptor.cpp.

use std::collections::VecDeque;

use boa_gc::{Finalize, Trace};
use bun_js::class::{Attributes, ClassBuilder, ClassRef};
use bun_js::{JSGlobalObject, JSValue, JsClass, JsResult};

use super::internals::{self, Reaction, invalid_state, promise_call, upon};
use super::readable_stream::{self, State};
use super::readable_stream_byob_reader::{self as byob_reader, ReadIntoRequest};
use super::readable_stream_default_reader::{ReadRequest, reader_of};
use crate::engine;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ReaderType {
    Default,
    Byob,
    None,
}

/// A pull-into descriptor: a buffer a read fills.
#[derive(Clone, Trace, Finalize)]
#[boa_gc(unsafe_no_drop)]
pub struct PullInto {
    buffer: JSValue,
    buffer_byte_length: usize,
    byte_offset: usize,
    byte_length: usize,
    bytes_filled: usize,
    minimum_fill: usize,
    element_size: usize,
    view_constructor: JSValue,
    #[unsafe_ignore_trace]
    reader_type: ReaderType,
}

#[derive(Clone, Trace, Finalize)]
#[boa_gc(unsafe_no_drop)]
struct QueueEntry {
    buffer: JSValue,
    byte_offset: usize,
    byte_length: usize,
}

#[derive(Trace, Finalize)]
pub struct ReadableByteStreamController {
    stream: JSValue,
    auto_allocate_chunk_size: Option<usize>,
    byob_request: JSValue,
    source: JSValue,
    pull: JSValue,
    cancel: JSValue,
    close_requested: bool,
    pull_again: bool,
    pulling: bool,
    pending_pull_intos: VecDeque<PullInto>,
    queue: VecDeque<QueueEntry>,
    queue_total_size: usize,
    started: bool,
    high_water_mark: f64,
}

pub fn controller_of(value: &JSValue) -> ClassRef<ReadableByteStreamController> {
    value
        .as_class_ref::<ReadableByteStreamController>()
        .expect("a ReadableByteStreamController")
}

pub fn is_byte_controller(value: &JSValue) -> bool {
    value.as_class_ref::<ReadableByteStreamController>().is_some()
}

fn stream_of_controller(global: &JSGlobalObject, controller: &JSValue) -> JsResult<JSValue> {
    Ok(controller_of(controller).borrow(global)?.stream.clone())
}

fn type_error(global: &JSGlobalObject, message: &str) -> bun_js::JsError {
    global.throw_type_error(format_args!("{message}"))
}

/// `SetUpReadableByteStreamControllerFromUnderlyingSource`.
pub fn set_up_from_underlying_source(
    global: &JSGlobalObject,
    stream: &JSValue,
    source: &JSValue,
    start: JSValue,
    pull: JSValue,
    cancel: JSValue,
    high_water_mark: f64,
    auto_allocate_chunk_size: Option<usize>,
) -> JsResult<()> {
    let controller = bun_js::class::to_js(
        global,
        ReadableByteStreamController {
            stream: stream.clone(),
            auto_allocate_chunk_size,
            byob_request: JSValue::NULL,
            source: source.clone(),
            pull,
            cancel,
            close_requested: false,
            pull_again: false,
            pulling: false,
            pending_pull_intos: VecDeque::new(),
            queue: VecDeque::new(),
            queue_total_size: 0,
            started: false,
            high_water_mark,
        },
    )?;
    readable_stream::stream_of(stream).borrow_mut(global)?.controller = controller.clone();
    let start_result = match start.is_callable() {
        true => start.call(global, source.clone(), std::slice::from_ref(&controller))?,
        false => JSValue::UNDEFINED,
    };
    let start_promise = internals::resolved(global, start_result);
    let on_started: Reaction<JSValue> = |global, controller, _| {
        controller_of(controller).borrow_mut(global)?.started = true;
        call_pull_if_needed(global, controller)?;
        Ok(JSValue::UNDEFINED)
    };
    let on_failed: Reaction<JSValue> = |global, controller, reason| {
        error(global, controller, reason)?;
        Ok(JSValue::UNDEFINED)
    };
    upon(global, &start_promise, controller, Some(on_started), Some(on_failed));
    Ok(())
}

fn has_default_reader(global: &JSGlobalObject, stream: &JSValue) -> JsResult<bool> {
    let reader = readable_stream::stream_of(stream).borrow(global)?.reader.clone();
    Ok(reader.as_class_ref::<super::readable_stream_default_reader::ReadableStreamDefaultReader>().is_some())
}

fn has_byob_reader(global: &JSGlobalObject, stream: &JSValue) -> JsResult<bool> {
    let reader = readable_stream::stream_of(stream).borrow(global)?.reader.clone();
    Ok(byob_reader::is_byob_reader(&reader))
}

/// `ReadableByteStreamControllerShouldCallPull`.
fn should_call_pull(global: &JSGlobalObject, controller: &JSValue) -> JsResult<bool> {
    let (stream, close_requested, started) = {
        let c = controller_of(controller);
        let c = c.borrow(global)?;
        (c.stream.clone(), c.close_requested, c.started)
    };
    if readable_stream::state(global, &stream)? != State::Readable || close_requested || !started {
        return Ok(false);
    }
    if has_default_reader(global, &stream)? && readable_stream::num_read_requests(global, &stream)? > 0 {
        return Ok(true);
    }
    if has_byob_reader(global, &stream)? && byob_reader::num_read_into_requests(global, &stream)? > 0 {
        return Ok(true);
    }
    Ok(desired_size(global, controller)?.is_some_and(|size| size > 0.0))
}

/// `ReadableByteStreamControllerCallPullIfNeeded`.
pub fn call_pull_if_needed(global: &JSGlobalObject, controller: &JSValue) -> JsResult<()> {
    if !should_call_pull(global, controller)? {
        return Ok(());
    }
    let (pull, source) = {
        let c = controller_of(controller);
        let mut c = c.borrow_mut(global)?;
        if c.pulling {
            c.pull_again = true;
            return Ok(());
        }
        c.pulling = true;
        (c.pull.clone(), c.source.clone())
    };
    let pulled = promise_call(global, &pull, &source, std::slice::from_ref(controller))?;
    let on_pulled: Reaction<JSValue> = |global, controller, _| {
        let again = {
            let c = controller_of(controller);
            let mut c = c.borrow_mut(global)?;
            c.pulling = false;
            std::mem::take(&mut c.pull_again)
        };
        if again {
            call_pull_if_needed(global, controller)?;
        }
        Ok(JSValue::UNDEFINED)
    };
    let on_failed: Reaction<JSValue> = |global, controller, reason| {
        error(global, controller, reason)?;
        Ok(JSValue::UNDEFINED)
    };
    upon(global, &pulled, controller.clone(), Some(on_pulled), Some(on_failed));
    Ok(())
}

fn clear_algorithms(global: &JSGlobalObject, controller: &JSValue) -> JsResult<()> {
    let c = controller_of(controller);
    let mut c = c.borrow_mut(global)?;
    c.pull = JSValue::UNDEFINED;
    c.cancel = JSValue::UNDEFINED;
    Ok(())
}

/// `ReadableByteStreamControllerInvalidateBYOBRequest`.
fn invalidate_byob_request(global: &JSGlobalObject, controller: &JSValue) -> JsResult<()> {
    let request = std::mem::replace(&mut controller_of(controller).borrow_mut(global)?.byob_request, JSValue::NULL);
    if let Some(request) = request.as_class_ref::<ReadableStreamBYOBRequest>() {
        let mut request = request.borrow_mut(global)?;
        request.controller = JSValue::UNDEFINED;
        request.view = JSValue::NULL;
    }
    Ok(())
}

fn clear_pending_pull_intos(global: &JSGlobalObject, controller: &JSValue) -> JsResult<()> {
    invalidate_byob_request(global, controller)?;
    controller_of(controller).borrow_mut(global)?.pending_pull_intos.clear();
    Ok(())
}

fn reset_queue(global: &JSGlobalObject, controller: &JSValue) -> JsResult<()> {
    let c = controller_of(controller);
    let mut c = c.borrow_mut(global)?;
    c.queue.clear();
    c.queue_total_size = 0;
    Ok(())
}

/// `ReadableByteStreamControllerClose`.
pub fn close(global: &JSGlobalObject, controller: &JSValue) -> JsResult<()> {
    let (stream, close_requested, total, first) = {
        let c = controller_of(controller);
        let c = c.borrow(global)?;
        (c.stream.clone(), c.close_requested, c.queue_total_size, c.pending_pull_intos.front().cloned())
    };
    if close_requested || readable_stream::state(global, &stream)? != State::Readable {
        return Ok(());
    }
    if total > 0 {
        controller_of(controller).borrow_mut(global)?.close_requested = true;
        return Ok(());
    }
    if let Some(first) = first
        && first.bytes_filled % first.element_size != 0
    {
        let reason = internals::type_error(global, "Insufficient bytes to fill elements in the given buffer");
        error(global, controller, reason.clone())?;
        return Err(global.throw_value(reason));
    }
    clear_algorithms(global, controller)?;
    readable_stream::close(global, &stream)
}

/// `ReadableByteStreamControllerError`.
pub fn error(global: &JSGlobalObject, controller: &JSValue, reason: JSValue) -> JsResult<()> {
    let stream = stream_of_controller(global, controller)?;
    if readable_stream::state(global, &stream)? != State::Readable {
        return Ok(());
    }
    clear_pending_pull_intos(global, controller)?;
    reset_queue(global, controller)?;
    clear_algorithms(global, controller)?;
    readable_stream::error(global, &stream, reason)
}

/// `ReadableByteStreamControllerGetDesiredSize`.
pub fn desired_size(global: &JSGlobalObject, controller: &JSValue) -> JsResult<Option<f64>> {
    let (stream, high_water_mark, total) = {
        let c = controller_of(controller);
        let c = c.borrow(global)?;
        (c.stream.clone(), c.high_water_mark, c.queue_total_size)
    };
    Ok(match readable_stream::state(global, &stream)? {
        State::Errored => None,
        State::Closed => Some(0.0),
        State::Readable => Some(high_water_mark - total as f64),
    })
}

fn enqueue_chunk_to_queue(global: &JSGlobalObject, controller: &JSValue, buffer: JSValue, byte_offset: usize, byte_length: usize) -> JsResult<()> {
    let c = controller_of(controller);
    let mut c = c.borrow_mut(global)?;
    c.queue.push_back(QueueEntry {
        buffer,
        byte_offset,
        byte_length,
    });
    c.queue_total_size += byte_length;
    Ok(())
}

fn enqueue_cloned_chunk_to_queue(global: &JSGlobalObject, controller: &JSValue, buffer: &JSValue, byte_offset: usize, byte_length: usize) -> JsResult<()> {
    let clone = engine::clone_array_buffer(global, buffer, byte_offset, byte_length)?;
    enqueue_chunk_to_queue(global, controller, clone, 0, byte_length)
}

fn shift_pending_pull_into(global: &JSGlobalObject, controller: &JSValue) -> JsResult<Option<PullInto>> {
    Ok(controller_of(controller).borrow_mut(global)?.pending_pull_intos.pop_front())
}

fn enqueue_detached_pull_into_to_queue(global: &JSGlobalObject, controller: &JSValue, descriptor: &PullInto) -> JsResult<()> {
    if descriptor.bytes_filled > 0 {
        enqueue_cloned_chunk_to_queue(global, controller, &descriptor.buffer, descriptor.byte_offset, descriptor.bytes_filled)?;
    }
    shift_pending_pull_into(global, controller)?;
    Ok(())
}

/// `ReadableByteStreamControllerEnqueue`.
pub fn enqueue(global: &JSGlobalObject, controller: &JSValue, chunk: &JSValue) -> JsResult<()> {
    let (stream, close_requested) = {
        let c = controller_of(controller);
        let c = c.borrow(global)?;
        (c.stream.clone(), c.close_requested)
    };
    if close_requested || readable_stream::state(global, &stream)? != State::Readable {
        return Ok(());
    }
    let Some(view) = engine::view_info(global, chunk)? else {
        return Err(type_error(global, "chunk must be an ArrayBufferView"));
    };
    if engine::is_detached(&view.buffer) {
        return Err(type_error(global, "chunk's buffer is detached"));
    }
    let transferred = engine::transfer_array_buffer(global, &view.buffer)?;
    let first = controller_of(controller).borrow(global)?.pending_pull_intos.front().cloned();
    if let Some(mut first) = first {
        if engine::is_detached(&first.buffer) {
            return Err(type_error(global, "The BYOB request's buffer has been detached"));
        }
        invalidate_byob_request(global, controller)?;
        first.buffer = engine::transfer_array_buffer(global, &first.buffer)?;
        if let Some(head) = controller_of(controller).borrow_mut(global)?.pending_pull_intos.front_mut() {
            head.buffer = first.buffer.clone();
        }
        if first.reader_type == ReaderType::None {
            enqueue_detached_pull_into_to_queue(global, controller, &first)?;
        }
    }
    if has_default_reader(global, &stream)? {
        process_read_requests_using_queue(global, controller)?;
        if readable_stream::num_read_requests(global, &stream)? == 0 {
            enqueue_chunk_to_queue(global, controller, transferred, view.byte_offset, view.byte_length)?;
        } else {
            if !controller_of(controller).borrow(global)?.pending_pull_intos.is_empty() {
                shift_pending_pull_into(global, controller)?;
            }
            let uint8 = engine::uint8_array_constructor(global);
            let transferred_view = engine::make_view(global, &uint8, &transferred, view.byte_offset, view.byte_length)?;
            readable_stream::fulfill_read_request(global, &stream, transferred_view, false)?;
        }
    } else if has_byob_reader(global, &stream)? {
        enqueue_chunk_to_queue(global, controller, transferred, view.byte_offset, view.byte_length)?;
        for filled in process_pull_into_descriptors_using_queue(global, controller)? {
            commit_pull_into_descriptor(global, &stream, filled)?;
        }
    } else {
        enqueue_chunk_to_queue(global, controller, transferred, view.byte_offset, view.byte_length)?;
    }
    call_pull_if_needed(global, controller)
}

/// `ReadableByteStreamControllerFillPullIntoDescriptorFromQueue`: whether it is filled enough.
fn fill_pull_into_descriptor_from_queue(global: &JSGlobalObject, controller: &JSValue, descriptor: &mut PullInto) -> JsResult<bool> {
    let c = controller_of(controller);
    let mut c = c.borrow_mut(global)?;
    let max_bytes_to_copy = c.queue_total_size.min(descriptor.byte_length - descriptor.bytes_filled);
    let max_bytes_filled = descriptor.bytes_filled + max_bytes_to_copy;
    let mut remaining = max_bytes_to_copy;
    let mut ready = false;
    let remainder = max_bytes_filled % descriptor.element_size;
    let max_aligned_bytes = max_bytes_filled - remainder;
    if max_aligned_bytes >= descriptor.minimum_fill {
        remaining = max_aligned_bytes - descriptor.bytes_filled;
        ready = true;
    }
    while remaining > 0 {
        let Some(head) = c.queue.front_mut() else { break };
        let bytes_to_copy = remaining.min(head.byte_length);
        let dest_start = descriptor.byte_offset + descriptor.bytes_filled;
        engine::copy_buffer_bytes(&head.buffer, head.byte_offset, &descriptor.buffer, dest_start, bytes_to_copy);
        if head.byte_length == bytes_to_copy {
            c.queue.pop_front();
        } else {
            head.byte_offset += bytes_to_copy;
            head.byte_length -= bytes_to_copy;
        }
        c.queue_total_size -= bytes_to_copy;
        descriptor.bytes_filled += bytes_to_copy;
        remaining -= bytes_to_copy;
    }
    Ok(ready)
}

/// `ReadableByteStreamControllerFillReadRequestFromQueue`.
fn fill_read_request_from_queue(global: &JSGlobalObject, controller: &JSValue, request: ReadRequest) -> JsResult<()> {
    let entry = {
        let c = controller_of(controller);
        let mut c = c.borrow_mut(global)?;
        let entry = c.queue.pop_front().expect("a queued chunk");
        c.queue_total_size -= entry.byte_length;
        entry
    };
    handle_queue_drain(global, controller)?;
    let uint8 = engine::uint8_array_constructor(global);
    let view = engine::make_view(global, &uint8, &entry.buffer, entry.byte_offset, entry.byte_length)?;
    request.chunk_steps(global, view)
}

/// `ReadableByteStreamControllerHandleQueueDrain`.
fn handle_queue_drain(global: &JSGlobalObject, controller: &JSValue) -> JsResult<()> {
    let (total, close_requested, stream) = {
        let c = controller_of(controller);
        let c = c.borrow(global)?;
        (c.queue_total_size, c.close_requested, c.stream.clone())
    };
    if total == 0 && close_requested {
        clear_algorithms(global, controller)?;
        return readable_stream::close(global, &stream);
    }
    call_pull_if_needed(global, controller)
}

/// `ReadableByteStreamControllerProcessReadRequestsUsingQueue`.
fn process_read_requests_using_queue(global: &JSGlobalObject, controller: &JSValue) -> JsResult<()> {
    let stream = stream_of_controller(global, controller)?;
    loop {
        if controller_of(controller).borrow(global)?.queue_total_size == 0 {
            return Ok(());
        }
        let reader = readable_stream::stream_of(&stream).borrow(global)?.reader.clone();
        let request = reader_of(&reader).borrow_mut(global)?.read_requests.pop_front();
        let Some(request) = request else { return Ok(()) };
        fill_read_request_from_queue(global, controller, request)?;
    }
}

/// `ReadableByteStreamControllerProcessPullIntoDescriptorsUsingQueue`.
fn process_pull_into_descriptors_using_queue(global: &JSGlobalObject, controller: &JSValue) -> JsResult<Vec<PullInto>> {
    let mut filled = Vec::new();
    loop {
        let first = {
            let c = controller_of(controller);
            let c = c.borrow(global)?;
            if c.queue_total_size == 0 {
                break;
            }
            c.pending_pull_intos.front().cloned()
        };
        let Some(mut descriptor) = first else { break };
        let ready = fill_pull_into_descriptor_from_queue(global, controller, &mut descriptor)?;
        if let Some(head) = controller_of(controller).borrow_mut(global)?.pending_pull_intos.front_mut() {
            *head = descriptor.clone();
        }
        if ready {
            shift_pending_pull_into(global, controller)?;
            filled.push(descriptor);
        } else {
            break;
        }
    }
    Ok(filled)
}

/// `ReadableByteStreamControllerConvertPullIntoDescriptor`.
fn convert_pull_into_descriptor(global: &JSGlobalObject, descriptor: &PullInto) -> JsResult<JSValue> {
    let buffer = engine::transfer_array_buffer(global, &descriptor.buffer)?;
    engine::make_view(
        global,
        &descriptor.view_constructor,
        &buffer,
        descriptor.byte_offset,
        descriptor.bytes_filled / descriptor.element_size,
    )
}

/// `ReadableByteStreamControllerCommitPullIntoDescriptor`.
fn commit_pull_into_descriptor(global: &JSGlobalObject, stream: &JSValue, descriptor: PullInto) -> JsResult<()> {
    let done = readable_stream::state(global, stream)? == State::Closed;
    let filled_view = convert_pull_into_descriptor(global, &descriptor)?;
    match descriptor.reader_type {
        ReaderType::Default => readable_stream::fulfill_read_request(global, stream, filled_view, done),
        _ => byob_reader::fulfill_read_into_request(global, stream, filled_view, done),
    }
}

/// `[[CancelSteps]](reason)`.
pub fn cancel_steps(global: &JSGlobalObject, controller: &JSValue, reason: JSValue) -> JsResult<JSValue> {
    clear_pending_pull_intos(global, controller)?;
    reset_queue(global, controller)?;
    let (cancel, source) = {
        let c = controller_of(controller);
        let c = c.borrow(global)?;
        (c.cancel.clone(), c.source.clone())
    };
    let result = promise_call(global, &cancel, &source, &[reason])?;
    clear_algorithms(global, controller)?;
    Ok(result)
}

/// `[[PullSteps]](readRequest)`.
pub fn pull_steps(global: &JSGlobalObject, controller: &JSValue, request: ReadRequest) -> JsResult<()> {
    let (stream, total, auto_allocate) = {
        let c = controller_of(controller);
        let c = c.borrow(global)?;
        (c.stream.clone(), c.queue_total_size, c.auto_allocate_chunk_size)
    };
    if total > 0 {
        return fill_read_request_from_queue(global, controller, request);
    }
    if let Some(size) = auto_allocate {
        let buffer = engine::new_array_buffer(global, size)?;
        controller_of(controller).borrow_mut(global)?.pending_pull_intos.push_back(PullInto {
            buffer,
            buffer_byte_length: size,
            byte_offset: 0,
            byte_length: size,
            bytes_filled: 0,
            minimum_fill: 1,
            element_size: 1,
            view_constructor: engine::uint8_array_constructor(global),
            reader_type: ReaderType::Default,
        });
    }
    readable_stream::add_read_request(global, &stream, request)?;
    call_pull_if_needed(global, controller)
}

/// `[[ReleaseSteps]]()`.
pub fn release_steps(global: &JSGlobalObject, controller: &JSValue) -> JsResult<()> {
    let c = controller_of(controller);
    let mut c = c.borrow_mut(global)?;
    if let Some(mut first) = c.pending_pull_intos.pop_front() {
        first.reader_type = ReaderType::None;
        c.pending_pull_intos.clear();
        c.pending_pull_intos.push_back(first);
    }
    Ok(())
}

/// `ReadableByteStreamControllerPullInto`.
pub fn pull_into(global: &JSGlobalObject, controller: &JSValue, view: &JSValue, min: usize, request: ReadIntoRequest) -> JsResult<()> {
    let stream = stream_of_controller(global, controller)?;
    let Some(info) = engine::view_info(global, view)? else {
        return request.error_steps(global, internals::type_error(global, "view must be an ArrayBufferView"));
    };
    let buffer = match engine::transfer_array_buffer(global, &info.buffer) {
        Ok(buffer) => buffer,
        Err(thrown) => return request.error_steps(global, internals::catch(global, thrown)?),
    };
    let mut descriptor = PullInto {
        buffer_byte_length: engine::array_buffer_byte_length(&buffer),
        buffer,
        byte_offset: info.byte_offset,
        byte_length: info.byte_length,
        bytes_filled: 0,
        minimum_fill: min * info.element_size,
        element_size: info.element_size,
        view_constructor: info.constructor,
        reader_type: ReaderType::Byob,
    };
    if !controller_of(controller).borrow(global)?.pending_pull_intos.is_empty() {
        controller_of(controller).borrow_mut(global)?.pending_pull_intos.push_back(descriptor);
        return byob_reader::add_read_into_request(global, &stream, request);
    }
    if readable_stream::state(global, &stream)? == State::Closed {
        let empty = engine::make_view(global, &descriptor.view_constructor, &descriptor.buffer, descriptor.byte_offset, 0)?;
        return request.close_steps(global, empty);
    }
    if controller_of(controller).borrow(global)?.queue_total_size > 0 {
        if fill_pull_into_descriptor_from_queue(global, controller, &mut descriptor)? {
            let filled_view = convert_pull_into_descriptor(global, &descriptor)?;
            handle_queue_drain(global, controller)?;
            return request.chunk_steps(global, filled_view);
        }
        if controller_of(controller).borrow(global)?.close_requested {
            let reason = internals::type_error(global, "Insufficient bytes to fill elements in the given buffer");
            error(global, controller, reason.clone())?;
            return request.error_steps(global, reason);
        }
    }
    controller_of(controller).borrow_mut(global)?.pending_pull_intos.push_back(descriptor);
    byob_reader::add_read_into_request(global, &stream, request)?;
    call_pull_if_needed(global, controller)
}

/// `ReadableByteStreamControllerRespond`.
fn respond(global: &JSGlobalObject, controller: &JSValue, bytes_written: usize) -> JsResult<()> {
    let stream = stream_of_controller(global, controller)?;
    let first = controller_of(controller).borrow(global)?.pending_pull_intos.front().cloned();
    let Some(first) = first else {
        return Err(type_error(global, "There is no pending BYOB request"));
    };
    if readable_stream::state(global, &stream)? == State::Closed {
        if bytes_written != 0 {
            return Err(type_error(global, "bytesWritten must be 0 when calling respond() on a closed stream"));
        }
    } else {
        if bytes_written == 0 {
            return Err(type_error(global, "bytesWritten must be greater than 0 when calling respond() on a readable stream"));
        }
        if first.bytes_filled + bytes_written > first.byte_length {
            return Err(global.throw_range_error(format_args!("bytesWritten out of range")));
        }
    }
    let buffer = engine::transfer_array_buffer(global, &first.buffer)?;
    if let Some(head) = controller_of(controller).borrow_mut(global)?.pending_pull_intos.front_mut() {
        head.buffer = buffer;
    }
    respond_internal(global, controller, bytes_written)
}

/// `ReadableByteStreamControllerRespondInternal`.
fn respond_internal(global: &JSGlobalObject, controller: &JSValue, bytes_written: usize) -> JsResult<()> {
    let stream = stream_of_controller(global, controller)?;
    invalidate_byob_request(global, controller)?;
    if readable_stream::state(global, &stream)? == State::Closed {
        respond_in_closed_state(global, controller)?;
    } else {
        respond_in_readable_state(global, controller, bytes_written)?;
    }
    call_pull_if_needed(global, controller)
}

/// `ReadableByteStreamControllerRespondInClosedState`.
fn respond_in_closed_state(global: &JSGlobalObject, controller: &JSValue) -> JsResult<()> {
    let first = controller_of(controller).borrow(global)?.pending_pull_intos.front().cloned();
    if first.is_some_and(|first| first.reader_type == ReaderType::None) {
        shift_pending_pull_into(global, controller)?;
    }
    let stream = stream_of_controller(global, controller)?;
    if has_byob_reader(global, &stream)? {
        while byob_reader::num_read_into_requests(global, &stream)? > 0 {
            let Some(descriptor) = shift_pending_pull_into(global, controller)? else { break };
            commit_pull_into_descriptor(global, &stream, descriptor)?;
        }
    }
    Ok(())
}

/// `ReadableByteStreamControllerRespondInReadableState`.
fn respond_in_readable_state(global: &JSGlobalObject, controller: &JSValue, bytes_written: usize) -> JsResult<()> {
    let stream = stream_of_controller(global, controller)?;
    let descriptor = {
        let c = controller_of(controller);
        let mut c = c.borrow_mut(global)?;
        let Some(head) = c.pending_pull_intos.front_mut() else { return Ok(()) };
        head.bytes_filled += bytes_written;
        head.clone()
    };
    if descriptor.reader_type == ReaderType::None {
        enqueue_detached_pull_into_to_queue(global, controller, &descriptor)?;
        for filled in process_pull_into_descriptors_using_queue(global, controller)? {
            commit_pull_into_descriptor(global, &stream, filled)?;
        }
        return Ok(());
    }
    if descriptor.bytes_filled < descriptor.minimum_fill {
        return Ok(());
    }
    let mut descriptor = shift_pending_pull_into(global, controller)?.expect("the head descriptor");
    let remainder = descriptor.bytes_filled % descriptor.element_size;
    if remainder > 0 {
        let end = descriptor.byte_offset + descriptor.bytes_filled;
        enqueue_cloned_chunk_to_queue(global, controller, &descriptor.buffer, end - remainder, remainder)?;
    }
    descriptor.bytes_filled -= remainder;
    let filled = process_pull_into_descriptors_using_queue(global, controller)?;
    commit_pull_into_descriptor(global, &stream, descriptor)?;
    for filled in filled {
        commit_pull_into_descriptor(global, &stream, filled)?;
    }
    Ok(())
}

/// `ReadableByteStreamControllerRespondWithNewView`.
fn respond_with_new_view(global: &JSGlobalObject, controller: &JSValue, view: &JSValue) -> JsResult<()> {
    let stream = stream_of_controller(global, controller)?;
    let first = controller_of(controller).borrow(global)?.pending_pull_intos.front().cloned();
    let Some(first) = first else {
        return Err(type_error(global, "There is no pending BYOB request"));
    };
    let Some(info) = engine::view_info(global, view)? else {
        return Err(type_error(global, "view must be an ArrayBufferView"));
    };
    if engine::is_detached(&info.buffer) {
        return Err(type_error(global, "view's buffer is detached"));
    }
    if readable_stream::state(global, &stream)? == State::Closed {
        if info.byte_length != 0 {
            return Err(type_error(global, "The view's length must be 0 when calling respondWithNewView() on a closed stream"));
        }
    } else if info.byte_length == 0 {
        return Err(type_error(global, "The view's length must be greater than 0 when calling respondWithNewView() on a readable stream"));
    }
    if first.byte_offset + first.bytes_filled != info.byte_offset {
        return Err(global.throw_range_error(format_args!("The region specified by view does not match byobRequest")));
    }
    if first.buffer_byte_length != engine::array_buffer_byte_length(&info.buffer) {
        return Err(global.throw_range_error(format_args!("The buffer of view has different capacity than byobRequest")));
    }
    if first.bytes_filled + info.byte_length > first.byte_length {
        return Err(global.throw_range_error(format_args!("The region specified by view is larger than byobRequest")));
    }
    let buffer = engine::transfer_array_buffer(global, &info.buffer)?;
    if let Some(head) = controller_of(controller).borrow_mut(global)?.pending_pull_intos.front_mut() {
        head.buffer = buffer;
    }
    respond_internal(global, controller, info.byte_length)
}

/// `ReadableByteStreamControllerGetBYOBRequest`.
fn get_byob_request(global: &JSGlobalObject, controller: &JSValue) -> JsResult<JSValue> {
    let (request, first) = {
        let c = controller_of(controller);
        let c = c.borrow(global)?;
        (c.byob_request.clone(), c.pending_pull_intos.front().cloned())
    };
    if !request.is_null() {
        return Ok(request);
    }
    let Some(first) = first else { return Ok(JSValue::NULL) };
    let uint8 = engine::uint8_array_constructor(global);
    let view = engine::make_view(
        global,
        &uint8,
        &first.buffer,
        first.byte_offset + first.bytes_filled,
        first.byte_length - first.bytes_filled,
    )?;
    let request = bun_js::class::to_js(
        global,
        ReadableStreamBYOBRequest {
            controller: controller.clone(),
            view,
        },
    )?;
    controller_of(controller).borrow_mut(global)?.byob_request = request.clone();
    Ok(request)
}

impl JsClass for ReadableByteStreamController {
    const NAME: &'static str = "ReadableByteStreamController";

    fn define(class: &mut ClassBuilder<'_, Self>) -> JsResult<()> {
        let attributes = Attributes::DEFAULT;
        class.getter("byobRequest", attributes, |this, global| get_byob_request(global, &this.to_js()))?;
        class.getter("desiredSize", attributes, |this, global| {
            Ok(match desired_size(global, &this.to_js())? {
                Some(size) => JSValue::js_number(size),
                None => JSValue::NULL,
            })
        })?;
        class.method("close", 0, attributes, |this, global, _| {
            let controller = this.to_js();
            let (close_requested, stream) = {
                let c = this.borrow(global)?;
                (c.close_requested, c.stream.clone())
            };
            if close_requested || readable_stream::state(global, &stream)? != State::Readable {
                return Err(global.throw_value(invalid_state(global, "Controller is already closed")));
            }
            close(global, &controller)?;
            Ok(JSValue::UNDEFINED)
        })?;
        class.method("enqueue", 1, attributes, |this, global, frame| {
            let controller = this.to_js();
            let chunk = frame.argument(0);
            let Some(info) = engine::view_info(global, &chunk)? else {
                return Err(type_error(global, "chunk must be an ArrayBufferView"));
            };
            if info.byte_length == 0 {
                return Err(type_error(global, "chunk must have a non-zero byteLength"));
            }
            if engine::array_buffer_byte_length(&info.buffer) == 0 {
                return Err(type_error(global, "chunk's buffer must have a non-zero byteLength"));
            }
            let (close_requested, stream) = {
                let c = this.borrow(global)?;
                (c.close_requested, c.stream.clone())
            };
            if close_requested || readable_stream::state(global, &stream)? != State::Readable {
                return Err(global.throw_value(invalid_state(global, "Controller is already closed")));
            }
            enqueue(global, &controller, &chunk)?;
            Ok(JSValue::UNDEFINED)
        })?;
        class.method("error", 1, attributes, |this, global, frame| {
            error(global, &this.to_js(), frame.argument(0))?;
            Ok(JSValue::UNDEFINED)
        })?;
        Ok(())
    }
}

#[derive(Trace, Finalize)]
pub struct ReadableStreamBYOBRequest {
    controller: JSValue,
    view: JSValue,
}

impl JsClass for ReadableStreamBYOBRequest {
    const NAME: &'static str = "ReadableStreamBYOBRequest";

    fn define(class: &mut ClassBuilder<'_, Self>) -> JsResult<()> {
        let attributes = Attributes::DEFAULT;
        class.getter("view", attributes, |this, global| Ok(this.borrow(global)?.view.clone()))?;
        class.method("respond", 1, attributes, |this, global, frame| {
            let (controller, view) = {
                let r = this.borrow(global)?;
                (r.controller.clone(), r.view.clone())
            };
            if controller.is_undefined() {
                return Err(type_error(global, "This BYOB request has been invalidated"));
            }
            let bytes_written = frame.argument(0).to_number(global)?;
            if !(bytes_written >= 0.0) || bytes_written.is_infinite() {
                return Err(global.throw_type_error(format_args!("bytesWritten must be a non-negative integer")));
            }
            if let Some(info) = engine::view_info(global, &view)?
                && engine::is_detached(&info.buffer)
            {
                return Err(type_error(global, "The BYOB request's buffer has been detached"));
            }
            respond(global, &controller, bytes_written as usize)?;
            Ok(JSValue::UNDEFINED)
        })?;
        class.method("respondWithNewView", 1, attributes, |this, global, frame| {
            let controller = this.borrow(global)?.controller.clone();
            if controller.is_undefined() {
                return Err(type_error(global, "This BYOB request has been invalidated"));
            }
            respond_with_new_view(global, &controller, &frame.argument(0))?;
            Ok(JSValue::UNDEFINED)
        })?;
        Ok(())
    }
}
