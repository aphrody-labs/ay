//! The natives of src/js/node/async_hooks.ts that the JavaScriptCore build has in src/jsc/bindings/NodeAsyncHooks.cpp.

use bun_js::{CallFrame, JSFunction, JSGlobalObject, JSValue, JsResult};

/// `jsSetAsyncHooksEnabled(isEnabled)`: called when an AsyncLocalStorage or an AsyncResource is constructed. Boa
/// compiles no code ahead of time, so either argument turns tracking on, as it does under the JIT: from then on
/// promise jobs and timers run in the async context they were queued in.
fn set_async_hooks_enabled(global: &JSGlobalObject, _: &CallFrame<'_>) -> JsResult<JSValue> {
    global.enable_async_context_tracking();
    Ok(JSValue::UNDEFINED)
}

/// What `$newCppFunction("NodeAsyncHooks.cpp", symbol, length)` answers.
pub fn native(global: &JSGlobalObject, symbol: &str) -> Option<JsResult<JSValue>> {
    match symbol {
        "jsSetAsyncHooksEnabled" => Some(Ok(JSFunction::create(global, "jsSetAsyncHooksEnabled", set_async_hooks_enabled, 1))),
        _ => None,
    }
}
