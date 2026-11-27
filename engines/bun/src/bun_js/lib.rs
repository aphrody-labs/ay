//! The JavaScript engine boundary: what Bun's Rust code calls instead of `bun_jsc`, on Boa.
//!
//! The names and shapes follow `bun_jsc` (`JSValue`, `JSGlobalObject`, `CallFrame`, `JSHostFn`, `Strong`/`Weak`,
//! `JsError`/`JsResult`, `TopExceptionScope`, `bindgen::{JsHeap, JsScope, Result}`), so that a crate moves from one to
//! the other by its imports. As in JavaScriptCore, an exception is pending on the global object: a function that
//! throws records it there and returns `Err(JsError::Thrown)`, and the host function boundary hands it to Boa.
//!
//! What differs, because Boa is not JavaScriptCore:
//! - `JSValue` is `Clone`, not `Copy`: it holds a reference to a GC cell. Its methods take `&self`.
//! - A string comes out as a Rust `String` (`as_string`, `to_utf8`), not a `JSString` cell.
//! - A class's payload is borrowed for a call through a `ClassRef`, and a call that re-enters one that borrows it
//!   mutably throws a `TypeError` instead.
//! - A GC root is any value Rust owns: Boa counts the references the heap does not account for. `Strong` is a
//!   value Rust keeps; `Weak` holds an object without keeping it alive.

#![warn(unused_must_use)]

mod array_buffer;
mod async_context;
pub mod bindgen;
mod call_frame;
pub mod class;
mod deep_equals;
mod error_code;
mod exception;
mod global;
mod host_fn;
mod internal_field;
pub mod module;
mod object;
mod promise;
mod realm;
mod strong;
mod value;
mod value_ext;

pub use array_buffer::{ArrayBuffer, StringUnits, TypedArrayType};
pub use async_context::{AsyncContextJobExecutor, carry_async_context};
pub use bun_core::JsError;
pub use call_frame::CallFrame;
pub use class::{ClassRef, JsClass};
pub use deep_equals::DeepEqualsMode;
pub use error_code::{ErrorCode, ErrorKind};
pub use exception::TopExceptionScope;
pub use global::JSGlobalObject;
pub use host_fn::{JSFunction, JSHostFn};
pub use module::JSModule;
pub use object::{JSArray, JSObject};
pub use promise::{JSPromise, PendingPromise};
pub use strong::{Strong, Weak};
pub use value::{CoerceTo, JSValue};

/// The result of anything that can throw: the exception is pending on the global object.
pub type JsResult<T> = core::result::Result<T, JsError>;

/// Boa itself, for an embedder that needs what this crate does not wrap yet.
pub use boa_engine as engine;
