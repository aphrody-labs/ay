//! `ReadableStreamDefaultTee` (Streams §4.9.1): JSStreamTeeState.cpp, the state that both branches share.

use boa_gc::{Finalize, Trace};
use bun_js::class::{ClassBuilder, ClassRef};
use bun_js::{JSArray, JSGlobalObject, JSValue, JsClass, JsResult};

use super::internals::{self, Deferred, Reaction, upon};
use super::readable_stream::{self, stream_of};
use super::readable_stream_default_controller::{self as controller, SourceAlgorithms};
use super::readable_stream_default_reader::{self as reader, ReadRequest};

#[derive(Trace, Finalize)]
pub struct StreamTeeState {
    stream: JSValue,
    reader: JSValue,
    reading: bool,
    read_again: bool,
    canceled: [bool; 2],
    reasons: [JSValue; 2],
    branches: [JSValue; 2],
    cancel_promise: Deferred,
}

impl JsClass for StreamTeeState {
    const NAME: &'static str = "StreamTeeState";

    fn define(_: &mut ClassBuilder<'_, Self>) -> JsResult<()> {
        Ok(())
    }
}

fn state_of(value: &JSValue) -> ClassRef<StreamTeeState> {
    value.as_class_ref::<StreamTeeState>().expect("a StreamTeeState")
}

fn branch_controller(global: &JSGlobalObject, state: &JSValue, branch: usize) -> JsResult<JSValue> {
    let stream = state_of(state).borrow(global)?.branches[branch].clone();
    Ok(stream_of(&stream).borrow(global)?.controller.clone())
}

/// `ReadableStreamDefaultTee(stream, false)`.
pub fn default_tee(global: &JSGlobalObject, stream: &JSValue) -> JsResult<[JSValue; 2]> {
    let reader = reader::acquire(global, stream)?;
    let state = bun_js::class::to_js(
        global,
        StreamTeeState {
            stream: stream.clone(),
            reader: reader.clone(),
            reading: false,
            read_again: false,
            canceled: [false; 2],
            reasons: [JSValue::UNDEFINED, JSValue::UNDEFINED],
            branches: [JSValue::UNDEFINED, JSValue::UNDEFINED],
            cancel_promise: Deferred::new(global),
        },
    )?;
    let branch = |index: u8| {
        readable_stream::create(
            global,
            SourceAlgorithms::Tee {
                state: state.clone(),
                branch: index,
            },
            1.0,
        )
    };
    let branches = [branch(0)?, branch(1)?];
    state_of(&state).borrow_mut(global)?.branches = branches.clone();
    let closed = reader::reader_of(&reader).borrow(global)?.closed.promise.clone();
    let on_errored: Reaction<JSValue> = |global, state, reason| {
        for branch in 0..2 {
            controller::error(global, &branch_controller(global, state, branch)?, reason.clone())?;
        }
        let (canceled, cancel_promise) = {
            let s = state_of(state);
            let s = s.borrow(global)?;
            (s.canceled, s.cancel_promise.clone())
        };
        if !canceled[0] || !canceled[1] {
            cancel_promise.resolve(global, JSValue::UNDEFINED);
        }
        Ok(JSValue::UNDEFINED)
    };
    upon(global, &closed, state, None, Some(on_errored));
    Ok(branches)
}

/// The branches' pull algorithm.
pub fn pull(global: &JSGlobalObject, state: &JSValue) -> JsResult<JSValue> {
    let reader = {
        let s = state_of(state);
        let mut s = s.borrow_mut(global)?;
        if s.reading {
            s.read_again = true;
            return Ok(internals::resolved(global, JSValue::UNDEFINED));
        }
        s.reading = true;
        s.reader.clone()
    };
    reader::read(global, &reader, ReadRequest::Tee(state.clone()))?;
    Ok(internals::resolved(global, JSValue::UNDEFINED))
}

/// A chunk: each branch that is not canceled gets it, in a microtask.
pub fn chunk_steps(global: &JSGlobalObject, state: &JSValue, chunk: JSValue) -> JsResult<()> {
    #[derive(Clone, Trace, Finalize)]
    #[boa_gc(unsafe_no_drop)]
    struct Step {
        state: JSValue,
        chunk: JSValue,
    }
    let step: Reaction<Step> = |global, step, _| {
        let canceled = {
            let s = state_of(&step.state);
            let mut s = s.borrow_mut(global)?;
            s.read_again = false;
            s.canceled
        };
        for branch in 0..2 {
            if !canceled[branch] {
                controller::enqueue(global, &branch_controller(global, &step.state, branch)?, step.chunk.clone())?;
            }
        }
        let again = {
            let s = state_of(&step.state);
            let mut s = s.borrow_mut(global)?;
            s.reading = false;
            s.read_again
        };
        if again {
            pull(global, &step.state)?;
        }
        Ok(JSValue::UNDEFINED)
    };
    let tick = internals::resolved(global, JSValue::UNDEFINED);
    upon(
        global,
        &tick,
        Step {
            state: state.clone(),
            chunk,
        },
        Some(step),
        None,
    );
    Ok(())
}

pub fn close_steps(global: &JSGlobalObject, state: &JSValue) -> JsResult<()> {
    let (canceled, cancel_promise) = {
        let s = state_of(state);
        let mut s = s.borrow_mut(global)?;
        s.reading = false;
        (s.canceled, s.cancel_promise.clone())
    };
    for branch in 0..2 {
        if !canceled[branch] {
            controller::close(global, &branch_controller(global, state, branch)?)?;
        }
    }
    if !canceled[0] || !canceled[1] {
        cancel_promise.resolve(global, JSValue::UNDEFINED);
    }
    Ok(())
}

pub fn error_steps(global: &JSGlobalObject, state: &JSValue) -> JsResult<()> {
    state_of(state).borrow_mut(global)?.reading = false;
    Ok(())
}

/// A branch's cancel algorithm: the source is canceled once both branches are, with both reasons.
pub fn cancel(global: &JSGlobalObject, state: &JSValue, branch: u8, reason: JSValue) -> JsResult<JSValue> {
    let branch = usize::from(branch);
    let (both, reasons, stream, cancel_promise) = {
        let s = state_of(state);
        let mut s = s.borrow_mut(global)?;
        s.canceled[branch] = true;
        s.reasons[branch] = reason;
        (
            s.canceled[0] && s.canceled[1],
            s.reasons.clone(),
            s.stream.clone(),
            s.cancel_promise.clone(),
        )
    };
    if both {
        let composite = JSArray::create(global, &reasons).to_js();
        let result = readable_stream::cancel(global, &stream, composite)?;
        cancel_promise.resolve(global, result);
    }
    Ok(cancel_promise.promise)
}
