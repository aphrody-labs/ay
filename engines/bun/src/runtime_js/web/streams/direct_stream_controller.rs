//! Bun's `type: "direct"` streams: JSDirectStreamController.cpp. The source's `pull(controller)` writes bytes with
//! `write()`, hands them over with `flush()`, and ends the stream with `end()` or `close(error?)`.
//!
//! Over a reader the bytes are the chunks of a default stream: what `pull` wrote is enqueued when it returns, when
//! its promise settles, at `flush()`, at the end, and at the end of a turn for a write outside `pull`. A one-shot
//! consumer (`text()`, `new Response(stream)`, `Bun.readableStreamTo*`) also takes the fulfillment of an async
//! `pull` as the end.

use boa_gc::{Finalize, Trace};
use bun_js::class::{Attributes, ClassBuilder, ClassRef};
use bun_js::{JSGlobalObject, JSValue, JsClass, JsResult};

use super::internals::{self, Reaction, upon};
use super::readable_stream;
use super::readable_stream_default_controller as controller;

#[derive(Trace, Finalize)]
pub struct DirectStreamController {
    stream: JSValue,
    source: JSValue,
    pull: JSValue,
    cancel: JSValue,
    close: JSValue,
    buffer: Vec<u8>,
    /// `end()`, `close()`, an error or a cancel: nothing more is delivered.
    closed: bool,
    /// The synchronous part of `pull` is running: its writes are delivered when it returns.
    in_pull: bool,
    /// A delivery at the end of the turn is pending, for a write outside `pull`.
    flush_scheduled: bool,
    /// A whole-body consumer reads it: `pull` runs once, and the end is `close()`, `end()` or its promise.
    one_shot: bool,
    pulled: bool,
}

impl JsClass for DirectStreamController {
    const NAME: &'static str = "DirectStreamController";

    fn define(class: &mut ClassBuilder<'_, Self>) -> JsResult<()> {
        let attributes = Attributes::DEFAULT;
        class.method("write", 1, attributes, |this, global, frame| write(global, &this.to_js(), frame.argument(0)))?;
        class.method("end", 0, attributes, |this, global, _| {
            finish_close(global, &this.to_js())?;
            Ok(JSValue::UNDEFINED)
        })?;
        class.method("close", 1, attributes, |this, global, frame| {
            let reason = frame.argument(0);
            if reason.is_truthy() {
                error(global, &this.to_js(), reason)?;
            } else {
                finish_close(global, &this.to_js())?;
            }
            Ok(JSValue::UNDEFINED)
        })?;
        class.method("flush", 0, attributes, |this, global, _| {
            deliver(global, &this.to_js())?;
            Ok(JSValue::UNDEFINED)
        })?;
        class.method("error", 1, attributes, |this, global, frame| {
            error(global, &this.to_js(), frame.argument(0))?;
            Ok(JSValue::UNDEFINED)
        })?;
        Ok(())
    }
}

fn direct_of(value: &JSValue) -> ClassRef<DirectStreamController> {
    value.as_class_ref::<DirectStreamController>().expect("a DirectStreamController")
}

/// The controller of a new direct stream; `pull`, `cancel` and `close` are the source's (`undefined` where absent).
pub fn create(
    global: &JSGlobalObject,
    stream: &JSValue,
    source: &JSValue,
    pull: JSValue,
    cancel: JSValue,
    close: JSValue,
) -> JsResult<JSValue> {
    bun_js::class::to_js(
        global,
        DirectStreamController {
            stream: stream.clone(),
            source: source.clone(),
            pull,
            cancel,
            close,
            buffer: Vec::new(),
            closed: false,
            in_pull: false,
            flush_scheduled: false,
            one_shot: false,
            pulled: false,
        },
    )
}

/// A consumer that reads the whole stream at once.
pub fn mark_one_shot(global: &JSGlobalObject, direct: &JSValue) -> JsResult<()> {
    direct_of(direct).borrow_mut(global)?.one_shot = true;
    Ok(())
}

fn stream_controller(global: &JSGlobalObject, direct: &JSValue) -> JsResult<JSValue> {
    let stream = direct_of(direct).borrow(global)?.stream.clone();
    Ok(readable_stream::stream_of(&stream).borrow(global)?.controller.clone())
}

/// `write(chunk)`: its bytes (a string is UTF-8) wait for the next delivery. Returns how many; 0 once closed.
fn write(global: &JSGlobalObject, direct: &JSValue, chunk: JSValue) -> JsResult<JSValue> {
    let bytes = match crate::engine::buffer_source_bytes(global, &chunk) {
        Some(bytes) => bytes,
        None if chunk.is_string() => chunk.to_utf8(global)?.into_bytes(),
        None if chunk.is_undefined_or_null() => Vec::new(),
        None => chunk.to_utf8(global)?.into_bytes(),
    };
    let schedule = {
        let d = direct_of(direct);
        let mut d = d.borrow_mut(global)?;
        if d.closed {
            return Ok(JSValue::js_number(0.0));
        }
        d.buffer.extend_from_slice(&bytes);
        let schedule = !d.in_pull && !d.flush_scheduled;
        if schedule {
            d.flush_scheduled = true;
        }
        schedule
    };
    if schedule {
        let at_end_of_turn: Reaction<JSValue> = |global, direct, _| {
            direct_of(direct).borrow_mut(global)?.flush_scheduled = false;
            deliver(global, direct)?;
            Ok(JSValue::UNDEFINED)
        };
        let tick = internals::resolved(global, JSValue::UNDEFINED);
        upon(global, &tick, direct.clone(), Some(at_end_of_turn), None);
    }
    Ok(JSValue::js_number(bytes.len() as f64))
}

/// Enqueues what was written as one `Uint8Array`, if anything was.
fn deliver(global: &JSGlobalObject, direct: &JSValue) -> JsResult<()> {
    let bytes = {
        let d = direct_of(direct);
        let mut d = d.borrow_mut(global)?;
        if d.buffer.is_empty() {
            return Ok(());
        }
        std::mem::take(&mut d.buffer)
    };
    let controller = stream_controller(global, direct)?;
    let chunk = crate::engine::uint8_array(global, &bytes)?;
    controller::enqueue(global, &controller, chunk)
}

fn run_close_hook(global: &JSGlobalObject, direct: &JSValue, reason: JSValue) -> JsResult<()> {
    let (close, source) = {
        let d = direct_of(direct);
        let d = d.borrow(global)?;
        (d.close.clone(), d.source.clone())
    };
    if close.is_callable() {
        close.call(global, source, &[reason])?;
    }
    Ok(())
}

/// `end()` and `close()`: what was written is delivered, then the stream closes.
fn finish_close(global: &JSGlobalObject, direct: &JSValue) -> JsResult<()> {
    if direct_of(direct).borrow(global)?.closed {
        return Ok(());
    }
    deliver(global, direct)?;
    direct_of(direct).borrow_mut(global)?.closed = true;
    let controller = stream_controller(global, direct)?;
    controller::close(global, &controller)?;
    run_close_hook(global, direct, JSValue::UNDEFINED)
}

/// `close(error)`, a throw of `pull`, or a rejection of its promise: the stream errors, unless it already ended.
fn error(global: &JSGlobalObject, direct: &JSValue, reason: JSValue) -> JsResult<()> {
    {
        let d = direct_of(direct);
        let mut d = d.borrow_mut(global)?;
        if d.closed {
            return Ok(());
        }
        d.closed = true;
        d.buffer.clear();
    }
    let controller = stream_controller(global, direct)?;
    controller::error(global, &controller, reason.clone())?;
    run_close_hook(global, direct, reason)
}

/// The default controller's pull algorithm: one call of the source's `pull`.
pub fn pull(global: &JSGlobalObject, direct: &JSValue) -> JsResult<JSValue> {
    let (pull, source, closed, once_more) = {
        let d = direct_of(direct);
        let mut d = d.borrow_mut(global)?;
        let once_more = d.one_shot && d.pulled;
        d.pulled = true;
        (d.pull.clone(), d.source.clone(), d.closed, once_more)
    };
    if closed || once_more {
        return Ok(internals::resolved(global, JSValue::UNDEFINED));
    }
    if !pull.is_callable() {
        let reason = internals::type_error(global, "underlyingSource.pull is not a function");
        error(global, direct, reason)?;
        return Ok(internals::resolved(global, JSValue::UNDEFINED));
    }
    direct_of(direct).borrow_mut(global)?.in_pull = true;
    let result = pull.call(global, source, std::slice::from_ref(direct));
    direct_of(direct).borrow_mut(global)?.in_pull = false;
    let result = match result {
        Ok(result) => result,
        Err(thrown) => {
            let reason = internals::catch(global, thrown)?;
            error(global, direct, reason)?;
            return Ok(internals::resolved(global, JSValue::UNDEFINED));
        }
    };
    deliver(global, direct)?;
    if !is_thenable(global, &result)? {
        return Ok(internals::resolved(global, JSValue::UNDEFINED));
    }
    let fulfilled: Reaction<JSValue> = |global, direct, _| {
        if direct_of(direct).borrow(global)?.closed {
            return Ok(JSValue::UNDEFINED);
        }
        deliver(global, direct)?;
        if direct_of(direct).borrow(global)?.one_shot {
            finish_close(global, direct)?;
        }
        Ok(JSValue::UNDEFINED)
    };
    let rejected: Reaction<JSValue> = |global, direct, reason| {
        error(global, direct, reason)?;
        Ok(JSValue::UNDEFINED)
    };
    Ok(internals::react(global, &result, direct.clone(), Some(fulfilled), Some(rejected)))
}

fn is_thenable(global: &JSGlobalObject, value: &JSValue) -> JsResult<bool> {
    if !value.is_object() {
        return Ok(false);
    }
    Ok(value.get(global, "then")?.is_some_and(|then| then.is_callable()))
}

/// The default controller's cancel algorithm: the source's `cancel(reason)`, unless the stream already ended.
pub fn cancel(global: &JSGlobalObject, direct: &JSValue, reason: JSValue) -> JsResult<JSValue> {
    let (cancel, source) = {
        let d = direct_of(direct);
        let mut d = d.borrow_mut(global)?;
        if d.closed {
            return Ok(internals::resolved(global, JSValue::UNDEFINED));
        }
        d.closed = true;
        d.buffer.clear();
        (d.cancel.clone(), d.source.clone())
    };
    internals::promise_call(global, &cancel, &source, &[reason])
}
