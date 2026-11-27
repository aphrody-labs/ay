//! `ReadableStreamDefaultController` (Streams §4.6, §4.9.4): JSReadableStreamDefaultController.cpp and its
//! operations in ReadableStreamOperations.cpp.

use boa_gc::{Finalize, Trace};
use bun_js::class::{Attributes, ClassBuilder, ClassRef};
use bun_js::{JSGlobalObject, JSValue, JsClass, JsResult};

use super::internals::{self, Queue, Reaction, chunk_size, invalid_state, promise_call, upon};
use super::readable_stream::{self, State};
use super::readable_stream_default_reader::ReadRequest;
use super::{direct_stream_controller, stream_tee_state, transform_stream};

/// Where a controller's pull and cancel algorithms come from.
#[derive(Clone, Trace, Finalize)]
#[boa_gc(unsafe_no_drop)]
pub enum SourceAlgorithms {
    /// Cleared, or a stream that is only enqueued to from Rust.
    None,
    /// An underlying source object's `pull` and `cancel` (`undefined` where it has none).
    Js { source: JSValue, pull: JSValue, cancel: JSValue },
    /// A branch of `tee()`.
    Tee { state: JSValue, branch: u8 },
    /// The readable side of a `TransformStream`.
    Transform { stream: JSValue },
    /// A `type: "direct"` source, through its `DirectStreamController`.
    Direct { controller: JSValue },
    /// `ReadableStream.from(iterable)`: the iterator, its `next`, and whether it is an async one.
    Iterator { iterator: JSValue, next: JSValue, is_async: bool },
    /// A descriptor's reads (`Bun.stdin.stream()`): a `FileSource`.
    File { source: JSValue },
}

/// The direct stream controller of `controller`, for a `type: "direct"` stream.
pub fn direct_controller(global: &JSGlobalObject, controller: &JSValue) -> JsResult<Option<JSValue>> {
    let Some(c) = controller.as_class_ref::<ReadableStreamDefaultController>() else {
        return Ok(None);
    };
    Ok(match &c.borrow(global)?.algorithms {
        SourceAlgorithms::Direct { controller } => Some(controller.clone()),
        _ => None,
    })
}

#[derive(Trace, Finalize)]
pub struct ReadableStreamDefaultController {
    pub(crate) stream: JSValue,
    queue: Queue,
    started: bool,
    close_requested: bool,
    pull_again: bool,
    pulling: bool,
    high_water_mark: f64,
    size: JSValue,
    algorithms: SourceAlgorithms,
}

pub fn controller_of(value: &JSValue) -> ClassRef<ReadableStreamDefaultController> {
    value
        .as_class_ref::<ReadableStreamDefaultController>()
        .expect("a ReadableStreamDefaultController")
}

fn pull_algorithm(global: &JSGlobalObject, controller: &JSValue, algorithms: &SourceAlgorithms) -> JsResult<JSValue> {
    match algorithms {
        SourceAlgorithms::None => Ok(internals::resolved(global, JSValue::UNDEFINED)),
        SourceAlgorithms::Js { source, pull, .. } => promise_call(global, pull, source, &[controller.clone()]),
        SourceAlgorithms::Tee { state, .. } => stream_tee_state::pull(global, state),
        SourceAlgorithms::Transform { stream } => transform_stream::source_pull(global, stream),
        SourceAlgorithms::Direct { controller } => direct_stream_controller::pull(global, controller),
        SourceAlgorithms::Iterator { iterator, next, is_async } => {
            readable_stream::iterator_pull(global, controller, iterator, next, *is_async)
        }
        SourceAlgorithms::File { source } => crate::webcore::blob::file_sink::source_pull(global, controller, source),
    }
}

fn cancel_algorithm(global: &JSGlobalObject, algorithms: &SourceAlgorithms, reason: JSValue) -> JsResult<JSValue> {
    match algorithms {
        SourceAlgorithms::None => Ok(internals::resolved(global, JSValue::UNDEFINED)),
        SourceAlgorithms::Js { source, cancel, .. } => promise_call(global, cancel, source, &[reason]),
        SourceAlgorithms::Tee { state, branch } => stream_tee_state::cancel(global, state, *branch, reason),
        SourceAlgorithms::Transform { stream } => transform_stream::source_cancel(global, stream, reason),
        SourceAlgorithms::Direct { controller } => direct_stream_controller::cancel(global, controller, reason),
        SourceAlgorithms::Iterator { iterator, .. } => readable_stream::iterator_cancel(global, iterator, reason),
        SourceAlgorithms::File { .. } => Ok(internals::resolved(global, JSValue::UNDEFINED)),
    }
}

/// `SetUpReadableStreamDefaultController`. `start` is the start algorithm's result (`undefined` for none).
pub fn set_up(
    global: &JSGlobalObject,
    stream: &JSValue,
    start: impl FnOnce(&JSValue) -> JsResult<JSValue>,
    algorithms: SourceAlgorithms,
    high_water_mark: f64,
    size: JSValue,
) -> JsResult<JSValue> {
    let controller = bun_js::class::to_js(
        global,
        ReadableStreamDefaultController {
            stream: stream.clone(),
            queue: Queue::default(),
            started: false,
            close_requested: false,
            pull_again: false,
            pulling: false,
            high_water_mark,
            size,
            algorithms,
        },
    )?;
    readable_stream::stream_of(stream).borrow_mut(global)?.controller = controller.clone();
    let start_result = start(&controller)?;
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
    upon(global, &start_promise, controller.clone(), Some(on_started), Some(on_failed));
    Ok(controller)
}

/// `ReadableStreamDefaultControllerCanCloseOrEnqueue`.
pub fn can_close_or_enqueue(global: &JSGlobalObject, controller: &JSValue) -> JsResult<bool> {
    let (close_requested, stream) = {
        let c = controller_of(controller);
        let c = c.borrow(global)?;
        (c.close_requested, c.stream.clone())
    };
    Ok(!close_requested && readable_stream::state(global, &stream)? == State::Readable)
}

/// `ReadableStreamDefaultControllerGetDesiredSize`: `None` for an errored stream.
pub fn desired_size(global: &JSGlobalObject, controller: &JSValue) -> JsResult<Option<f64>> {
    let c = controller_of(controller);
    let c = c.borrow(global)?;
    Ok(match readable_stream::state(global, &c.stream)? {
        State::Errored => None,
        State::Closed => Some(0.0),
        State::Readable => Some(c.high_water_mark - c.queue.total_size()),
    })
}

/// `ReadableStreamDefaultControllerShouldCallPull`.
fn should_call_pull(global: &JSGlobalObject, controller: &JSValue) -> JsResult<bool> {
    if !can_close_or_enqueue(global, controller)? {
        return Ok(false);
    }
    let (started, stream) = {
        let c = controller_of(controller);
        let c = c.borrow(global)?;
        (c.started, c.stream.clone())
    };
    if !started {
        return Ok(false);
    }
    if readable_stream::is_locked(global, &stream)? && readable_stream::num_read_requests(global, &stream)? > 0 {
        return Ok(true);
    }
    Ok(desired_size(global, controller)?.is_some_and(|size| size > 0.0))
}

/// `ReadableStreamDefaultControllerHasBackpressure`.
pub fn has_backpressure(global: &JSGlobalObject, controller: &JSValue) -> JsResult<bool> {
    Ok(!should_call_pull(global, controller)?)
}

/// `ReadableStreamDefaultControllerCallPullIfNeeded`.
pub fn call_pull_if_needed(global: &JSGlobalObject, controller: &JSValue) -> JsResult<()> {
    if !should_call_pull(global, controller)? {
        return Ok(());
    }
    let algorithms = {
        let c = controller_of(controller);
        let mut c = c.borrow_mut(global)?;
        if c.pulling {
            c.pull_again = true;
            return Ok(());
        }
        c.pulling = true;
        c.algorithms.clone()
    };
    let pull_promise = pull_algorithm(global, controller, &algorithms)?;
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
    upon(global, &pull_promise, controller.clone(), Some(on_pulled), Some(on_failed));
    Ok(())
}

/// `ReadableStreamDefaultControllerClearAlgorithms`.
fn clear_algorithms(global: &JSGlobalObject, controller: &JSValue) -> JsResult<()> {
    let c = controller_of(controller);
    let mut c = c.borrow_mut(global)?;
    c.algorithms = SourceAlgorithms::None;
    c.size = JSValue::UNDEFINED;
    Ok(())
}

/// `ReadableStreamDefaultControllerClose`.
pub fn close(global: &JSGlobalObject, controller: &JSValue) -> JsResult<()> {
    if !can_close_or_enqueue(global, controller)? {
        return Ok(());
    }
    let (empty, stream) = {
        let c = controller_of(controller);
        let mut c = c.borrow_mut(global)?;
        c.close_requested = true;
        (c.queue.is_empty(), c.stream.clone())
    };
    if empty {
        clear_algorithms(global, controller)?;
        readable_stream::close(global, &stream)?;
    }
    Ok(())
}

/// `ReadableStreamDefaultControllerEnqueue`: what the size algorithm or the queue throws errors the stream, and is
/// thrown.
pub fn enqueue(global: &JSGlobalObject, controller: &JSValue, chunk: JSValue) -> JsResult<()> {
    if !can_close_or_enqueue(global, controller)? {
        return Ok(());
    }
    let (stream, size) = {
        let c = controller_of(controller);
        let c = c.borrow(global)?;
        (c.stream.clone(), c.size.clone())
    };
    if readable_stream::is_locked(global, &stream)? && readable_stream::num_read_requests(global, &stream)? > 0 {
        readable_stream::fulfill_read_request(global, &stream, chunk, false)?;
    } else {
        let size = match chunk_size(global, &size, &chunk) {
            Ok(size) => size,
            Err(thrown) => return Err(error_and_rethrow(global, controller, thrown)?),
        };
        let queued = controller_of(controller).borrow_mut(global)?.queue.enqueue(global, chunk, size);
        if let Err(thrown) = queued {
            return Err(error_and_rethrow(global, controller, thrown)?);
        }
    }
    call_pull_if_needed(global, controller)
}

fn error_and_rethrow(global: &JSGlobalObject, controller: &JSValue, thrown: bun_js::JsError) -> JsResult<bun_js::JsError> {
    let reason = internals::catch(global, thrown)?;
    error(global, controller, reason.clone())?;
    Ok(global.throw_value(reason))
}

/// `ReadableStreamDefaultControllerError`.
pub fn error(global: &JSGlobalObject, controller: &JSValue, reason: JSValue) -> JsResult<()> {
    let stream = controller_of(controller).borrow(global)?.stream.clone();
    if readable_stream::state(global, &stream)? != State::Readable {
        return Ok(());
    }
    controller_of(controller).borrow_mut(global)?.queue.reset();
    clear_algorithms(global, controller)?;
    readable_stream::error(global, &stream, reason)
}

/// `[[CancelSteps]](reason)`.
pub fn cancel_steps(global: &JSGlobalObject, controller: &JSValue, reason: JSValue) -> JsResult<JSValue> {
    let algorithms = {
        let c = controller_of(controller);
        let mut c = c.borrow_mut(global)?;
        c.queue.reset();
        c.algorithms.clone()
    };
    let result = cancel_algorithm(global, &algorithms, reason)?;
    clear_algorithms(global, controller)?;
    Ok(result)
}

/// `[[PullSteps]](readRequest)`.
pub fn pull_steps(global: &JSGlobalObject, controller: &JSValue, read_request: ReadRequest) -> JsResult<()> {
    let (chunk, close_now, stream) = {
        let c = controller_of(controller);
        let mut c = c.borrow_mut(global)?;
        if c.queue.is_empty() {
            (None, false, c.stream.clone())
        } else {
            let chunk = c.queue.dequeue();
            (Some(chunk), c.close_requested && c.queue.is_empty(), c.stream.clone())
        }
    };
    match chunk {
        Some(chunk) => {
            if close_now {
                clear_algorithms(global, controller)?;
                readable_stream::close(global, &stream)?;
            } else {
                call_pull_if_needed(global, controller)?;
            }
            read_request.chunk_steps(global, chunk)
        }
        None => {
            readable_stream::add_read_request(global, &stream, read_request)?;
            call_pull_if_needed(global, controller)
        }
    }
}

impl JsClass for ReadableStreamDefaultController {
    const NAME: &'static str = "ReadableStreamDefaultController";

    fn define(class: &mut ClassBuilder<'_, Self>) -> JsResult<()> {
        let attributes = Attributes::DEFAULT;
        class.getter("desiredSize", attributes, |this, global| {
            Ok(match desired_size(global, &this.to_js())? {
                Some(size) => JSValue::js_number(size),
                None => JSValue::NULL,
            })
        })?;
        class.method("close", 0, attributes, |this, global, _| {
            let controller = this.to_js();
            if !can_close_or_enqueue(global, &controller)? {
                return Err(global.throw_value(invalid_state(global, "Controller is already closed")));
            }
            close(global, &controller)?;
            Ok(JSValue::UNDEFINED)
        })?;
        class.method("enqueue", 1, attributes, |this, global, frame| {
            let controller = this.to_js();
            if !can_close_or_enqueue(global, &controller)? {
                return Err(global.throw_value(invalid_state(global, "Controller is already closed")));
            }
            enqueue(global, &controller, frame.argument(0))?;
            Ok(JSValue::UNDEFINED)
        })?;
        class.method("error", 1, attributes, |this, global, frame| {
            error(global, &this.to_js(), frame.argument(0))?;
            Ok(JSValue::UNDEFINED)
        })?;
        Ok(())
    }
}
