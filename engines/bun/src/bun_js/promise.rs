//! `JSPromise`: the promises Rust makes, settled already or later (`JSPromise::create`).

use boa_engine::object::builtins::JsPromise;

use crate::{JSGlobalObject, JSValue, JsResult};

pub struct JSPromise;

impl JSPromise {
    /// `Promise.resolve(value)`: a thenable is followed.
    pub fn resolved_promise_value(global: &JSGlobalObject, value: JSValue) -> JSValue {
        match JsPromise::resolve(value.0, global.cx()) {
            Ok(promise) => JSValue(promise.into()),
            // A thenable whose `then` getter throws: the promise is rejected with that, as JavaScriptCore's is.
            Err(error) => {
                let reason = error.into_opaque(global.cx()).unwrap_or_default();
                Self::rejected_promise_value(global, JSValue(reason))
            }
        }
    }

    /// A promise rejected with `reason`.
    pub fn rejected_promise_value(global: &JSGlobalObject, reason: JSValue) -> JSValue {
        let (promise, resolvers) = JsPromise::new_pending(global.cx());
        let _ = resolvers
            .reject
            .call(&boa_engine::JsValue::undefined(), &[reason.0], global.cx());
        JSValue(promise.into())
    }

    /// A pending promise, and what settles it.
    pub fn create(global: &JSGlobalObject) -> PendingPromise {
        let (promise, resolvers) = JsPromise::new_pending(global.cx());
        PendingPromise {
            promise: JSValue(promise.into()),
            resolve: JSValue(resolvers.resolve.into()),
            reject: JSValue(resolvers.reject.into()),
        }
    }
}

/// A promise that Rust settles once: `resolve` and `reject` take it by value.
pub struct PendingPromise {
    promise: JSValue,
    resolve: JSValue,
    reject: JSValue,
}

impl PendingPromise {
    pub fn promise(&self) -> JSValue {
        self.promise.clone()
    }

    pub fn resolve(self, global: &JSGlobalObject, value: JSValue) -> JsResult<()> {
        self.resolve.call(global, JSValue::UNDEFINED, &[value]).map(drop)
    }

    pub fn reject(self, global: &JSGlobalObject, reason: JSValue) -> JsResult<()> {
        self.reject.call(global, JSValue::UNDEFINED, &[reason]).map(drop)
    }
}
