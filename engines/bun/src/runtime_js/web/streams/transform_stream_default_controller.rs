//! `TransformStreamDefaultController` (Streams §6.3, §6.4.2): JSTransformStreamDefaultController.cpp.

use boa_gc::{Finalize, Trace};
use bun_js::class::{Attributes, ClassBuilder, ClassRef};
use bun_js::{JSGlobalObject, JSValue, JsClass, JsResult};

use super::internals::{self, Deferred, Reaction, promise_call};
use super::readable_stream;
use super::readable_stream_default_controller as readable_controller;
use super::native_transform::{self, NativeTransform};
use super::transform_stream;

/// Where a controller's transform, flush and cancel algorithms come from.
#[derive(Clone, Trace, Finalize)]
#[boa_gc(unsafe_no_drop)]
pub enum TransformAlgorithms {
    /// Cleared.
    None,
    /// A transformer object's `transform`, `flush` and `cancel`; without `transform`, the identity.
    Js { transformer: JSValue, transform: JSValue, flush: JSValue, cancel: JSValue },
    /// Bun's own transformer, in Rust (`TextEncoderStream`, `CompressionStream`, ...).
    Native(NativeTransform),
}

/// Runs a step of a native transformer, then enqueues what it made.
fn native_step(
    global: &JSGlobalObject,
    controller: &JSValue,
    step: impl FnOnce(&mut NativeTransform) -> JsResult<Vec<Vec<u8>>>,
) -> JsResult<JSValue> {
    let taken = std::mem::replace(&mut controller_of(controller).borrow_mut(global)?.algorithms, TransformAlgorithms::None);
    let TransformAlgorithms::Native(mut state) = taken else {
        controller_of(controller).borrow_mut(global)?.algorithms = taken;
        return Ok(internals::resolved(global, JSValue::UNDEFINED));
    };
    let made = step(&mut state);
    let text = native_transform::yields_text(&state);
    controller_of(controller).borrow_mut(global)?.algorithms = TransformAlgorithms::Native(state);
    internals::promise_of(global, || {
        for chunk in made? {
            let chunk = match text {
                true => JSValue::from_utf8(&chunk),
                false => crate::engine::uint8_array(global, &chunk)?,
            };
            enqueue(global, controller, chunk)?;
        }
        Ok(JSValue::UNDEFINED)
    })
}

#[derive(Trace, Finalize)]
pub struct TransformStreamDefaultController {
    stream: JSValue,
    algorithms: TransformAlgorithms,
    pub(crate) finish: Option<Deferred>,
}

pub fn controller_of(value: &JSValue) -> ClassRef<TransformStreamDefaultController> {
    value
        .as_class_ref::<TransformStreamDefaultController>()
        .expect("a TransformStreamDefaultController")
}

fn stream_of_controller(global: &JSGlobalObject, controller: &JSValue) -> JsResult<JSValue> {
    Ok(controller_of(controller).borrow(global)?.stream.clone())
}

fn readable_controller_of(global: &JSGlobalObject, stream: &JSValue) -> JsResult<JSValue> {
    let readable = transform_stream::stream_of(stream).borrow(global)?.readable.clone();
    Ok(readable_stream::stream_of(&readable).borrow(global)?.controller.clone())
}

/// `SetUpTransformStreamDefaultController` with the algorithms of `SetUpTransformStreamDefaultControllerFromTransformer`.
pub fn set_up(global: &JSGlobalObject, stream: &JSValue, algorithms: TransformAlgorithms) -> JsResult<JSValue> {
    let controller = bun_js::class::to_js(
        global,
        TransformStreamDefaultController {
            stream: stream.clone(),
            algorithms,
            finish: None,
        },
    )?;
    transform_stream::stream_of(stream).borrow_mut(global)?.controller = controller.clone();
    Ok(controller)
}

/// `TransformStreamDefaultControllerClearAlgorithms`.
pub fn clear_algorithms(global: &JSGlobalObject, controller: &JSValue) -> JsResult<()> {
    controller_of(controller).borrow_mut(global)?.algorithms = TransformAlgorithms::None;
    Ok(())
}

pub fn cancel_algorithm(global: &JSGlobalObject, controller: &JSValue, reason: JSValue) -> JsResult<JSValue> {
    match controller_of(controller).borrow(global)?.algorithms.clone() {
        TransformAlgorithms::Js { transformer, cancel, .. } => promise_call(global, &cancel, &transformer, &[reason]),
        TransformAlgorithms::None | TransformAlgorithms::Native(_) => Ok(internals::resolved(global, JSValue::UNDEFINED)),
    }
}

pub fn flush_algorithm(global: &JSGlobalObject, controller: &JSValue) -> JsResult<JSValue> {
    let is_native = matches!(controller_of(controller).borrow(global)?.algorithms, TransformAlgorithms::Native(_));
    if is_native {
        return native_step(global, controller, |state| native_transform::flush(global, state));
    }
    match controller_of(controller).borrow(global)?.algorithms.clone() {
        TransformAlgorithms::Js { transformer, flush, .. } => {
            promise_call(global, &flush, &transformer, &[controller.clone()])
        }
        TransformAlgorithms::None | TransformAlgorithms::Native(_) => Ok(internals::resolved(global, JSValue::UNDEFINED)),
    }
}

fn transform_algorithm(global: &JSGlobalObject, controller: &JSValue, chunk: JSValue) -> JsResult<JSValue> {
    let is_native = matches!(controller_of(controller).borrow(global)?.algorithms, TransformAlgorithms::Native(_));
    if is_native {
        return native_step(global, controller, |state| native_transform::transform(global, state, &chunk));
    }
    match controller_of(controller).borrow(global)?.algorithms.clone() {
        TransformAlgorithms::Js { transformer, transform, .. } if transform.is_callable() => {
            promise_call(global, &transform, &transformer, &[chunk, controller.clone()])
        }
        _ => internals::promise_of(global, || {
            enqueue(global, controller, chunk)?;
            Ok(JSValue::UNDEFINED)
        }),
    }
}

/// `TransformStreamDefaultControllerEnqueue`.
pub fn enqueue(global: &JSGlobalObject, controller: &JSValue, chunk: JSValue) -> JsResult<()> {
    let stream = stream_of_controller(global, controller)?;
    let readable_controller = readable_controller_of(global, &stream)?;
    if !readable_controller::can_close_or_enqueue(global, &readable_controller)? {
        return Err(global.throw_type_error(format_args!(
            "Cannot enqueue a chunk into a TransformStream whose readable side is closed or has already requested close"
        )));
    }
    if let Err(thrown) = readable_controller::enqueue(global, &readable_controller, chunk) {
        let reason = internals::catch(global, thrown)?;
        transform_stream::error_writable_and_unblock_write(global, &stream, reason)?;
        let readable = transform_stream::stream_of(&stream).borrow(global)?.readable.clone();
        let stored = readable_stream::stream_of(&readable).borrow(global)?.stored_error.clone();
        return Err(global.throw_value(stored));
    }
    let backpressure = readable_controller::has_backpressure(global, &readable_controller)?;
    if Some(backpressure) != transform_stream::backpressure(global, &stream)? {
        transform_stream::set_backpressure(global, &stream, true)?;
    }
    Ok(())
}

/// `TransformStreamDefaultControllerPerformTransform`.
pub fn perform_transform(global: &JSGlobalObject, controller: &JSValue, chunk: JSValue) -> JsResult<JSValue> {
    let transformed = transform_algorithm(global, controller, chunk)?;
    let stream = stream_of_controller(global, controller)?;
    let failed: Reaction<JSValue> = |global, stream, reason| {
        transform_stream::error(global, stream, reason.clone())?;
        Err(global.throw_value(reason))
    };
    Ok(internals::react(global, &transformed, stream, None, Some(failed)))
}

/// `TransformStreamDefaultControllerTerminate`.
pub fn terminate(global: &JSGlobalObject, controller: &JSValue) -> JsResult<()> {
    let stream = stream_of_controller(global, controller)?;
    readable_controller::close(global, &readable_controller_of(global, &stream)?)?;
    let error = internals::type_error(global, "The TransformStream has been terminated");
    transform_stream::error_writable_and_unblock_write(global, &stream, error)
}

impl JsClass for TransformStreamDefaultController {
    const NAME: &'static str = "TransformStreamDefaultController";

    fn define(class: &mut ClassBuilder<'_, Self>) -> JsResult<()> {
        let attributes = Attributes::DEFAULT;
        class.getter("desiredSize", attributes, |this, global| {
            let stream = this.borrow(global)?.stream.clone();
            let readable_controller = readable_controller_of(global, &stream)?;
            Ok(match readable_controller::desired_size(global, &readable_controller)? {
                Some(size) => JSValue::js_number(size),
                None => JSValue::NULL,
            })
        })?;
        class.method("enqueue", 1, attributes, |this, global, frame| {
            enqueue(global, &this.to_js(), frame.argument(0))?;
            Ok(JSValue::UNDEFINED)
        })?;
        class.method("error", 1, attributes, |this, global, frame| {
            let stream = this.borrow(global)?.stream.clone();
            transform_stream::error(global, &stream, frame.argument(0))?;
            Ok(JSValue::UNDEFINED)
        })?;
        class.method("terminate", 0, attributes, |this, global, _| {
            terminate(global, &this.to_js())?;
            Ok(JSValue::UNDEFINED)
        })?;
        Ok(())
    }
}
