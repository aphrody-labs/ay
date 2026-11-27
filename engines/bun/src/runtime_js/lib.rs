#![warn(unused_must_use)]
// A weak `Bun__internal_dispatch_ready_poll` (socket/mod.rs).
#![feature(linkage)]
//! `bun_runtime`'s JS-visible APIs on `bun_js`, the engine boundary on Boa: the bodies of the `.bind.ts` schemas that
//! the JavaScriptCore build implements in `src/runtime`, over the same engine-free crates.
//!
//! [`generated_bindgen::native`] is what `$bindgen(file, symbol)` answers, [`cpp`] what `$cpp(file, symbol)` and
//! `$newCppFunction(file, symbol, length)` do, [`bun_object`] what else of `Bun` this crate implements.
//!
//! - [`event_loop`]: Boa's job executor with Bun's turn, on the thread's uSockets loop.
//! - [`socket`]: the `us_dispatch_*` of uSockets in Rust.
//! - [`timer`]: `setTimeout`, `setInterval`, `setImmediate`.
//! - `work_pool`: `EventLoop::run_on_work_pool`, what the async functions of node:fs run on.
//! - [`webcore`]: `Blob`, `Headers`, `Request`, `Response`, `fetch()` on `bun_http_rs`'s client.
//! - [`server`]: `Bun.serve()` on `bun_http_rs`'s server.

pub mod api;
pub mod crypto;
pub mod dispatch;
mod engine;
pub mod event_loop;
pub mod generated_bindgen;
pub mod node;
pub mod server;
pub mod socket;
pub mod sys_js;
pub mod test_runner;
pub mod timer;
pub mod web;
pub mod webcore;
mod work_pool;

use boa_engine::property::{Attribute, PropertyDescriptor};
use boa_engine::{Context, JsResult, JsString, JsValue};
use bun_js::{JSGlobalObject, JSValue};
// `posix_spawn_bun`, which `bun_spawn_sys::spawn_process_posix` calls and nothing names in Rust.
use bun_spawn_rs as _;

pub use event_loop::EventLoop;

/// The natives that the JavaScriptCore build has in C++ (`src/jsc/bindings/<file>`), ported: `None` for one that
/// is not.
pub fn cpp(global: &bun_js::JSGlobalObject, file: &str, symbol: &str) -> Option<bun_js::JsResult<bun_js::JSValue>> {
    // `$newCppFunction` names the wrapper that src/codegen/generate-js2native.ts writes around the C++ function.
    let symbol = symbol.strip_prefix("js2native_wrap_").unwrap_or(symbol);
    match file {
        "NodeAsyncHooks.cpp" => node::async_hooks::native(global, symbol),
        "NodeVM.cpp" => node::vm::native(global, symbol),
        _ => None,
    }
}

/// The properties of `Bun` this crate implements besides Blob's.
pub fn bun_object(global: &JSGlobalObject) -> bun_js::JsResult<Vec<(&'static str, JSValue)>> {
    let mut properties = generated_bindgen::glob::exports(global)?;
    properties.extend(generated_bindgen::js_bun_spawn_bindings::exports(global)?);
    properties.extend(api::bun_object::functions(global));
    properties.extend(web::streams::readable_stream::bun_functions(global));
    properties.extend(generated_bindgen::unsafe_object::namespaces(global)?);
    Ok(properties)
}

/// What of `process` this crate implements, by name: `process.kill`, `process._kill`.
pub fn process_object(global: &JSGlobalObject) -> Vec<(&'static str, JSValue)> {
    node::process::functions(global)
}

/// Boa's context of a realm, for one engine operation at a time (see `JSGlobalObject::context`).
#[allow(clippy::mut_from_ref)]
pub(crate) fn cx(global: &JSGlobalObject) -> &mut Context {
    // SAFETY: each use is one operation on the context, done before anything else reaches the realm.
    unsafe { global.context() }
}

/// A `bun_js` result as Boa's: the pending exception becomes the error.
pub(crate) fn lift<T>(global: &JSGlobalObject, result: bun_js::JsResult<T>) -> JsResult<T> {
    result.map_err(|error| global.into_engine_error(error))
}

pub(crate) fn define_global(context: &mut Context, name: &str, value: JsValue) -> JsResult<()> {
    let attribute = Attribute::WRITABLE | Attribute::CONFIGURABLE;
    let global = context.global_object();
    global.define_property_or_throw(
        JsString::from(name),
        PropertyDescriptor::builder()
            .value(value)
            .writable(attribute.writable())
            .enumerable(false)
            .configurable(attribute.configurable()),
        context,
    )?;
    Ok(())
}

/// Puts Bun's runtime globals in the realm, over Boa's WinterTC ones of the same names. The realm's job executor is
/// an [`EventLoop`].
pub fn install(context: &mut Context) -> JsResult<()> {
    timer::install(context)?;
    web::abort::install(context)?;
    web::streams::install(context)?;
    webcore::install(context)
}
