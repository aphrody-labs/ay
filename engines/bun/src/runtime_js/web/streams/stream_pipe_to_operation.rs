//! `ReadableStreamPipeTo` (Streams §4.9.1): JSStreamPipeToOperation.cpp, the state of one pipe.

use boa_gc::{Finalize, Trace};
use bun_js::class::{ClassBuilder, ClassRef};
use bun_js::{JSFunction, JSGlobalObject, JSValue, JsClass, JsResult};

use super::internals::{self, Deferred, Reaction, upon};
use super::readable_stream::{self, State as ReadableState};
use super::readable_stream_default_reader::{self as reader, ReadRequest};
use super::writable_stream::{self, State as WritableState};
use super::writable_stream_default_writer::{self as writer, writer_of};

/// `StreamPipeOptions`.
pub struct Options {
    pub prevent_abort: bool,
    pub prevent_cancel: bool,
    pub prevent_close: bool,
    pub signal: JSValue,
}

/// What a shutdown with an action does before it finalizes.
#[derive(Clone, Trace, Finalize)]
#[boa_gc(unsafe_no_drop)]
enum Action {
    AbortDestination(JSValue),
    CancelSource(JSValue),
    CloseDestination,
    AbortBoth(JSValue),
}

#[derive(Trace, Finalize)]
pub struct StreamPipeToOperation {
    source: JSValue,
    destination: JSValue,
    reader: JSValue,
    writer: JSValue,
    prevent_abort: bool,
    prevent_cancel: bool,
    prevent_close: bool,
    signal: JSValue,
    abort_listener: JSValue,
    shutting_down: bool,
    current_write: JSValue,
    shutdown: Option<Shutdown>,
    promise: Deferred,
}

impl JsClass for StreamPipeToOperation {
    const NAME: &'static str = "StreamPipeToOperation";

    fn define(_: &mut ClassBuilder<'_, Self>) -> JsResult<()> {
        Ok(())
    }
}

fn operation_of(value: &JSValue) -> ClassRef<StreamPipeToOperation> {
    value.as_class_ref::<StreamPipeToOperation>().expect("a StreamPipeToOperation")
}

/// `ReadableStreamPipeTo(source, dest, preventClose, preventAbort, preventCancel, signal)`.
pub fn pipe_to(global: &JSGlobalObject, source: &JSValue, destination: &JSValue, options: Options) -> JsResult<JSValue> {
    let reader = reader::acquire(global, source)?;
    let writer = writer::acquire(global, destination)?;
    readable_stream::stream_of(source).borrow_mut(global)?.disturbed = true;
    let promise = Deferred::new(global);
    let operation = bun_js::class::to_js(
        global,
        StreamPipeToOperation {
            source: source.clone(),
            destination: destination.clone(),
            reader: reader.clone(),
            writer: writer.clone(),
            prevent_abort: options.prevent_abort,
            prevent_cancel: options.prevent_cancel,
            prevent_close: options.prevent_close,
            signal: options.signal.clone(),
            abort_listener: JSValue::UNDEFINED,
            shutting_down: false,
            current_write: internals::resolved(global, JSValue::UNDEFINED),
            shutdown: None,
            promise: promise.clone(),
        },
    )?;
    if !options.signal.is_undefined() {
        if options.signal.get(global, "aborted")?.is_some_and(|aborted| aborted.to_boolean()) {
            abort_algorithm(global, &operation)?;
            return Ok(promise.promise);
        }
        let listener = JSFunction::create(global, "abort", |global, frame| {
            let operation = frame.this().get(global, "__pipe")?.unwrap_or_default();
            if operation.as_class_ref::<StreamPipeToOperation>().is_some() {
                abort_algorithm(global, &operation)?;
            }
            Ok(JSValue::UNDEFINED)
        }, 0);
        let bound = bind_listener(global, &listener, &operation)?;
        operation_of(&operation).borrow_mut(global)?.abort_listener = bound.clone();
        if let Some(add) = options.signal.get(global, "addEventListener")? {
            add.call(global, options.signal.clone(), &[JSValue::from_utf8(b"abort"), bound])?;
        }
    }
    watch(global, &operation)?;
    pipe_step(global, &operation)?;
    Ok(promise.promise)
}

/// The listener called with `this` an object that names the operation: `listener.bind({ __pipe: operation })`.
fn bind_listener(global: &JSGlobalObject, listener: &JSValue, operation: &JSValue) -> JsResult<JSValue> {
    let this = bun_js::JSObject::create(global);
    this.put(global, "__pipe", operation.clone());
    let bind = listener.get(global, "bind")?.unwrap_or_default();
    bind.call(global, listener.clone(), &[this.to_js()])
}

/// The abort algorithm of the pipe's signal: the error is the signal's reason.
fn abort_algorithm(global: &JSGlobalObject, operation: &JSValue) -> JsResult<()> {
    let (signal, destination, source, prevent_abort, prevent_cancel) = {
        let o = operation_of(operation);
        let o = o.borrow(global)?;
        (o.signal.clone(), o.destination.clone(), o.source.clone(), o.prevent_abort, o.prevent_cancel)
    };
    let error = signal.get(global, "reason")?.unwrap_or_default();
    let abort_destination = !prevent_abort && writable_stream::state(global, &destination)? == WritableState::Writable;
    let cancel_source = !prevent_cancel && readable_stream::state(global, &source)? == ReadableState::Readable;
    let action = match (abort_destination, cancel_source) {
        (true, true) => Some(Action::AbortBoth(error.clone())),
        (true, false) => Some(Action::AbortDestination(error.clone())),
        (false, true) => Some(Action::CancelSource(error.clone())),
        (false, false) => None,
    };
    match action {
        Some(action) => shutdown_with_action(global, operation, action, Some(error)),
        None => shutdown(global, operation, Some(error)),
    }
}

/// Reacts to the source and the destination closing or erroring.
fn watch(global: &JSGlobalObject, operation: &JSValue) -> JsResult<()> {
    let (reader, writer) = {
        let o = operation_of(operation);
        let o = o.borrow(global)?;
        (o.reader.clone(), o.writer.clone())
    };
    let source_closed = reader::reader_of(&reader).borrow(global)?.closed.promise.clone();
    let destination_closed = writer_of(&writer).borrow(global)?.closed.promise.clone();
    // Errors must be propagated forward: the source errors.
    let source_errored: Reaction<JSValue> = |global, operation, error| {
        if operation_of(operation).borrow(global)?.prevent_abort {
            shutdown(global, operation, Some(error))?;
        } else {
            shutdown_with_action(global, operation, Action::AbortDestination(error.clone()), Some(error))?;
        }
        Ok(JSValue::UNDEFINED)
    };
    // Closing must be propagated forward: the source closes.
    let source_closes: Reaction<JSValue> = |global, operation, _| {
        if operation_of(operation).borrow(global)?.prevent_close {
            shutdown(global, operation, None)?;
        } else {
            shutdown_with_action(global, operation, Action::CloseDestination, None)?;
        }
        Ok(JSValue::UNDEFINED)
    };
    upon(global, &source_closed, operation.clone(), Some(source_closes), Some(source_errored));
    // Errors must be propagated backward: the destination errors.
    let destination_errored: Reaction<JSValue> = |global, operation, error| {
        if operation_of(operation).borrow(global)?.prevent_cancel {
            shutdown(global, operation, Some(error))?;
        } else {
            shutdown_with_action(global, operation, Action::CancelSource(error.clone()), Some(error))?;
        }
        Ok(JSValue::UNDEFINED)
    };
    upon(global, &destination_closed, operation.clone(), None, Some(destination_errored));
    // Closing must be propagated backward: the destination is already closing or closed.
    let destination = operation_of(operation).borrow(global)?.destination.clone();
    if writable_stream::close_queued_or_in_flight(global, &destination)?
        || writable_stream::state(global, &destination)? == WritableState::Closed
    {
        let error = internals::type_error(
            global,
            "The destination WritableStream closed before all of the data could be piped to it",
        );
        if operation_of(operation).borrow(global)?.prevent_cancel {
            shutdown(global, operation, Some(error))?;
        } else {
            shutdown_with_action(global, operation, Action::CancelSource(error.clone()), Some(error))?;
        }
    }
    Ok(())
}

/// One turn of the pipe: once the destination is ready, read a chunk and write it.
fn pipe_step(global: &JSGlobalObject, operation: &JSValue) -> JsResult<()> {
    let (shutting_down, writer) = {
        let o = operation_of(operation);
        let o = o.borrow(global)?;
        (o.shutting_down, o.writer.clone())
    };
    if shutting_down {
        return Ok(());
    }
    let ready = writer_of(&writer).borrow(global)?.ready.promise.clone();
    let read: Reaction<JSValue> = |global, operation, _| {
        let (shutting_down, reader) = {
            let o = operation_of(operation);
            let o = o.borrow(global)?;
            (o.shutting_down, o.reader.clone())
        };
        if !shutting_down && !reader::reader_of(&reader).borrow(global)?.stream.is_undefined() {
            reader::read(global, &reader, ReadRequest::Pipe(operation.clone()))?;
        }
        Ok(JSValue::UNDEFINED)
    };
    upon(global, &ready, operation.clone(), Some(read), None);
    Ok(())
}

/// A chunk was read: write it, and go on.
pub fn chunk_steps(global: &JSGlobalObject, operation: &JSValue, chunk: JSValue) -> JsResult<()> {
    let writer = operation_of(operation).borrow(global)?.writer.clone();
    if writer_of(&writer).borrow(global)?.stream.is_undefined() {
        return Ok(());
    }
    let write = writer::write(global, &writer, chunk)?;
    internals::mark_handled(global, &write);
    operation_of(operation).borrow_mut(global)?.current_write = write;
    pipe_step(global, operation)
}

/// Waits for the write in progress, if the destination is still writable and not closing.
fn after_pending_writes(
    global: &JSGlobalObject,
    operation: &JSValue,
    next: Reaction<JSValue>,
) -> JsResult<()> {
    let (destination, current_write) = {
        let o = operation_of(operation);
        let o = o.borrow(global)?;
        (o.destination.clone(), o.current_write.clone())
    };
    let waits = writable_stream::state(global, &destination)? == WritableState::Writable
        && !writable_stream::close_queued_or_in_flight(global, &destination)?;
    if waits {
        upon(global, &current_write, operation.clone(), Some(next), Some(next));
        Ok(())
    } else {
        next(global, operation, JSValue::UNDEFINED).map(drop)
    }
}

#[derive(Clone, Trace, Finalize)]
#[boa_gc(unsafe_no_drop)]
struct Shutdown {
    operation: JSValue,
    action: Option<Action>,
    error: Option<JSValue>,
}

fn shutdown_state(global: &JSGlobalObject, operation: &JSValue) -> JsResult<bool> {
    let o = operation_of(operation);
    let mut o = o.borrow_mut(global)?;
    if o.shutting_down {
        return Ok(true);
    }
    o.shutting_down = true;
    Ok(false)
}

/// "Shutdown with an action": once pending writes are done, run the action, then finalize.
fn shutdown_with_action(global: &JSGlobalObject, operation: &JSValue, action: Action, error: Option<JSValue>) -> JsResult<()> {
    if shutdown_state(global, operation)? {
        return Ok(());
    }
    pending_shutdown(global, operation, Shutdown { operation: operation.clone(), action: Some(action), error })
}

/// "Shutdown": once pending writes are done, finalize.
fn shutdown(global: &JSGlobalObject, operation: &JSValue, error: Option<JSValue>) -> JsResult<()> {
    if shutdown_state(global, operation)? {
        return Ok(());
    }
    pending_shutdown(global, operation, Shutdown { operation: operation.clone(), action: None, error })
}

/// The shutdown waits in the operation for the pending writes.
fn pending_shutdown(global: &JSGlobalObject, operation: &JSValue, shutdown: Shutdown) -> JsResult<()> {
    operation_of(operation).borrow_mut(global)?.shutdown = Some(shutdown);
    let next: Reaction<JSValue> = |global, operation, _| {
        let shutdown = operation_of(operation).borrow_mut(global)?.shutdown.take();
        if let Some(shutdown) = shutdown {
            run_shutdown(global, &shutdown)?;
        }
        Ok(JSValue::UNDEFINED)
    };
    after_pending_writes(global, operation, next)
}

fn run_shutdown(global: &JSGlobalObject, shutdown: &Shutdown) -> JsResult<()> {
    let Some(action) = shutdown.action.clone() else {
        return finalize(global, &shutdown.operation, shutdown.error.clone());
    };
    let (source, destination, writer) = {
        let o = operation_of(&shutdown.operation);
        let o = o.borrow(global)?;
        (o.source.clone(), o.destination.clone(), o.writer.clone())
    };
    let promise = match action {
        Action::AbortDestination(error) => writable_stream::abort(global, &destination, error)?,
        Action::CancelSource(error) => readable_stream::cancel(global, &source, error)?,
        Action::CloseDestination => writer::close_with_error_propagation(global, &writer)?,
        Action::AbortBoth(error) => {
            let abort = writable_stream::abort(global, &destination, error.clone())?;
            let cancel = readable_stream::cancel(global, &source, error)?;
            crate::engine::promise_all(global, &[abort, cancel])?
        }
    };
    let done: Reaction<Shutdown> = |global, shutdown, _| {
        finalize(global, &shutdown.operation, shutdown.error.clone())?;
        Ok(JSValue::UNDEFINED)
    };
    let failed: Reaction<Shutdown> = |global, shutdown, error| {
        finalize(global, &shutdown.operation, Some(error))?;
        Ok(JSValue::UNDEFINED)
    };
    upon(global, &promise, shutdown.clone(), Some(done), Some(failed));
    Ok(())
}

/// "Finalize": release the writer and the reader, and settle the pipe's promise.
fn finalize(global: &JSGlobalObject, operation: &JSValue, error: Option<JSValue>) -> JsResult<()> {
    let (reader, writer, signal, listener, promise) = {
        let o = operation_of(operation);
        let o = o.borrow(global)?;
        (o.reader.clone(), o.writer.clone(), o.signal.clone(), o.abort_listener.clone(), o.promise.clone())
    };
    if !writer_of(&writer).borrow(global)?.stream.is_undefined() {
        writer::release(global, &writer)?;
    }
    if !reader::reader_of(&reader).borrow(global)?.stream.is_undefined() {
        reader::release(global, &reader)?;
    }
    if !listener.is_undefined()
        && let Some(remove) = signal.get(global, "removeEventListener")?
    {
        remove.call(global, signal, &[JSValue::from_utf8(b"abort"), listener])?;
    }
    match error {
        Some(error) => promise.reject(global, error),
        None => promise.resolve(global, JSValue::UNDEFINED),
    }
    Ok(())
}
