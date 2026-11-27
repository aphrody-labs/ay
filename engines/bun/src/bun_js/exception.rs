//! `TopExceptionScope`: where Rust looks at the exception that is pending, as in `bun_jsc`.

use crate::{JSGlobalObject, JSValue, JsError, JsResult};

/// The exception pending on a global object, seen from where Rust called into JavaScript.
pub struct TopExceptionScope<'a> {
    global: &'a JSGlobalObject,
}

impl<'a> TopExceptionScope<'a> {
    pub fn new(global: &'a JSGlobalObject) -> TopExceptionScope<'a> {
        TopExceptionScope { global }
    }

    pub fn has_exception(&self) -> bool {
        self.global.has_exception()
    }

    /// The pending exception, which stays pending.
    pub fn exception(&self) -> Option<JSValue> {
        self.global.peek_exception()
    }

    pub fn clear_exception(&self) {
        self.global.clear_exception();
    }

    /// `Err(JsError::Thrown)` if an exception is pending.
    pub fn return_if_exception(&self) -> JsResult<()> {
        if self.has_exception() {
            Err(JsError::Thrown)
        } else {
            Ok(())
        }
    }

    /// The error that says an exception is pending.
    pub fn err_for_pending(&self) -> JsError {
        debug_assert!(self.has_exception(), "no exception is pending");
        JsError::Thrown
    }

    pub fn assert_no_exception(&self) {
        debug_assert!(!self.has_exception(), "an exception is pending");
    }

    pub fn assert_exception_presence_matches(&self, should_have_exception: bool) {
        debug_assert_eq!(
            self.has_exception(),
            should_have_exception,
            "exception presence"
        );
    }
}
