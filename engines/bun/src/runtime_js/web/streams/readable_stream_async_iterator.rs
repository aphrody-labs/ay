//! `ReadableStream.prototype.values()` and `[Symbol.asyncIterator]` (Streams §4.2.5, WebIDL §3.7.10.2):
//! JSReadableStreamAsyncIterator.cpp.

use boa_gc::{Finalize, Trace};
use bun_js::class::{Attributes, ClassBuilder, ClassRef};
use bun_js::{JSGlobalObject, JSValue, JsClass, JsResult};

use super::internals::{self, Deferred, Reaction, iter_result};
use super::readable_stream;
use super::readable_stream_default_reader::{self as reader, ReadRequest};

#[derive(Trace, Finalize)]
pub struct ReadableStreamAsyncIterator {
    reader: JSValue,
    prevent_cancel: bool,
    ongoing: JSValue,
    finished: bool,
}

impl JsClass for ReadableStreamAsyncIterator {
    const NAME: &'static str = "ReadableStreamAsyncIterator";

    fn define(class: &mut ClassBuilder<'_, Self>) -> JsResult<()> {
        let hidden = Attributes {
            writable: true,
            enumerable: false,
            configurable: true,
        };
        class.method("next", 0, hidden, |this, global, _| next(global, &this.to_js()))?;
        class.method("return", 1, hidden, |this, global, frame| {
            return_(global, &this.to_js(), frame.argument(0))
        })?;
        Ok(())
    }
}

fn iterator_of(value: &JSValue) -> ClassRef<ReadableStreamAsyncIterator> {
    value
        .as_class_ref::<ReadableStreamAsyncIterator>()
        .expect("a ReadableStreamAsyncIterator")
}

/// The iterator of `values({ preventCancel })`: the stream's reader is acquired now.
pub fn create(global: &JSGlobalObject, stream: &JSValue, prevent_cancel: bool) -> JsResult<JSValue> {
    let reader = reader::acquire(global, stream)?;
    let constructor = bun_js::class::constructor::<ReadableStreamAsyncIterator>(global)?;
    let prototype = constructor.get(global, "prototype")?.unwrap_or_default();
    crate::engine::inherit_async_iterator_prototype(global, &prototype);
    bun_js::class::to_js(
        global,
        ReadableStreamAsyncIterator {
            reader,
            prevent_cancel,
            ongoing: JSValue::UNDEFINED,
            finished: false,
        },
    )
}

/// "Get the next iteration result": the promise of `{ value, done }`.
fn next_iteration_result(global: &JSGlobalObject, iterator: &JSValue) -> JsResult<JSValue> {
    let reader = iterator_of(iterator).borrow(global)?.reader.clone();
    let deferred = Deferred::new(global);
    reader::read(
        global,
        &reader,
        ReadRequest::Iterator {
            reader: reader.clone(),
            deferred: deferred.clone(),
        },
    )?;
    Ok(deferred.promise)
}

/// The next steps, which run once the ongoing promise has settled.
fn next_steps(global: &JSGlobalObject, iterator: &JSValue, _: JSValue) -> JsResult<JSValue> {
    if iterator_of(iterator).borrow(global)?.finished {
        return Ok(internals::resolved(global, iter_result(global, JSValue::UNDEFINED, true)));
    }
    let next = next_iteration_result(global, iterator)?;
    let fulfilled: Reaction<JSValue> = |global, iterator, result| {
        let done = result.get(global, "done")?.is_some_and(|done| done.to_boolean());
        let i = iterator_of(iterator);
        let mut i = i.borrow_mut(global)?;
        i.ongoing = JSValue::UNDEFINED;
        if done {
            i.finished = true;
        }
        Ok(result)
    };
    let rejected: Reaction<JSValue> = |global, iterator, reason| {
        let i = iterator_of(iterator);
        let mut i = i.borrow_mut(global)?;
        i.ongoing = JSValue::UNDEFINED;
        i.finished = true;
        Err(global.throw_value(reason))
    };
    let ongoing = internals::react(global, &next, iterator.clone(), Some(fulfilled), Some(rejected));
    iterator_of(iterator).borrow_mut(global)?.ongoing = ongoing.clone();
    Ok(ongoing)
}

fn next(global: &JSGlobalObject, iterator: &JSValue) -> JsResult<JSValue> {
    let ongoing = iterator_of(iterator).borrow(global)?.ongoing.clone();
    if ongoing.is_undefined() {
        return next_steps(global, iterator, JSValue::UNDEFINED);
    }
    let after: Reaction<JSValue> = |global, iterator, value| next_steps(global, iterator, value);
    let chained = internals::react(global, &ongoing, iterator.clone(), Some(after), Some(after));
    iterator_of(iterator).borrow_mut(global)?.ongoing = chained.clone();
    Ok(chained)
}

#[derive(Clone, Trace, Finalize)]
#[boa_gc(unsafe_no_drop)]
struct Return {
    iterator: JSValue,
    value: JSValue,
}

/// The return steps: the stream is canceled with `value` unless `preventCancel`, and the reader released.
fn return_steps(global: &JSGlobalObject, step: &Return, _: JSValue) -> JsResult<JSValue> {
    let (finished, reader, prevent_cancel) = {
        let i = iterator_of(&step.iterator);
        let mut i = i.borrow_mut(global)?;
        let finished = i.finished;
        i.finished = true;
        (finished, i.reader.clone(), i.prevent_cancel)
    };
    if finished {
        return Ok(internals::resolved(global, iter_result(global, step.value.clone(), true)));
    }
    let stream = reader::reader_of(&reader).borrow(global)?.stream.clone();
    if stream.is_undefined() {
        return Ok(internals::resolved(global, JSValue::UNDEFINED));
    }
    if !prevent_cancel {
        let result = readable_stream::cancel(global, &stream, step.value.clone())?;
        reader::release(global, &reader)?;
        return Ok(result);
    }
    reader::release(global, &reader)?;
    Ok(internals::resolved(global, JSValue::UNDEFINED))
}

fn return_(global: &JSGlobalObject, iterator: &JSValue, value: JSValue) -> JsResult<JSValue> {
    let step = Return {
        iterator: iterator.clone(),
        value,
    };
    let ongoing = iterator_of(iterator).borrow(global)?.ongoing.clone();
    let returned = if ongoing.is_undefined() {
        return_steps(global, &step, JSValue::UNDEFINED)?
    } else {
        let after: Reaction<Return> = return_steps;
        internals::react(global, &ongoing, step.clone(), Some(after), Some(after))
    };
    iterator_of(iterator).borrow_mut(global)?.ongoing = returned.clone();
    let done: Reaction<Return> = |global, step, _| Ok(iter_result(global, step.value.clone(), true));
    Ok(internals::react(global, &returned, step, Some(done), None))
}
