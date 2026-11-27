//! `CallFrame`: what a host function is called with.

use boa_engine::JsValue;

use crate::JSValue;

/// `this` and the arguments of a call.
pub struct CallFrame<'a> {
    this: &'a JSValue,
    arguments: &'a [JSValue],
}

impl<'a> CallFrame<'a> {
    pub(crate) fn new(this: &'a JsValue, arguments: &'a [JsValue]) -> CallFrame<'a> {
        CallFrame {
            // SAFETY: `JSValue` is `repr(transparent)` over `JsValue`.
            this: unsafe { &*(core::ptr::from_ref(this) as *const JSValue) },
            arguments: JSValue::from_engine_slice(arguments),
        }
    }

    /// `this`. For a constructor Boa calls, the `new.target`.
    pub fn this(&self) -> &'a JSValue {
        self.this
    }

    pub fn arguments(&self) -> &'a [JSValue] {
        self.arguments
    }

    pub fn arguments_count(&self) -> usize {
        self.arguments.len()
    }

    /// The argument at `index`, `undefined` if the call has fewer.
    pub fn argument(&self, index: usize) -> JSValue {
        self.arguments.get(index).cloned().unwrap_or_default()
    }

    /// The first `N` arguments, `undefined` for each the call does not have.
    pub fn arguments_as_array<const N: usize>(&self) -> [JSValue; N] {
        core::array::from_fn(|index| self.argument(index))
    }
}
