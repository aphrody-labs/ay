//! `JSGlobalObject`: a realm (Boa's `Context`) and the state Bun keeps beside it: the pending exception, the
//! prototypes of the classes Rust defines, the native modules.

use core::any::TypeId;
use core::cell::UnsafeCell;
use core::fmt;
use std::collections::HashMap;
use std::rc::Rc;

use boa_engine::error::{EngineError, PanicError, RuntimeLimitError};
use boa_engine::module::IdleModuleLoader;
use boa_engine::{Context, JsNativeError, JsObject, JsValue, Source};

use crate::error_code::ErrorKind;
use crate::module::{Loader, NativeModules};
use crate::{JSValue, JsError, JsResult};

/// A realm. It is only ever seen behind a reference: `&JSGlobalObject` is what every function that touches
/// JavaScript takes, as in `bun_jsc`.
#[repr(transparent)]
pub struct JSGlobalObject(UnsafeCell<Context>);

#[derive(Default)]
pub(crate) struct State {
    /// Boa's own error, not its value: an uncatchable one (a runtime limit) stays uncatchable.
    exception: Option<boa_engine::JsError>,
    pub(crate) prototypes: HashMap<TypeId, JsObject>,
    /// The prototype of each `ErrorCode`'s errors, by code.
    pub(crate) error_prototypes: HashMap<crate::ErrorCode, JsObject>,
    pub(crate) modules: NativeModules,
    /// What [`JSGlobalObject::cached`] made, by the type that names it.
    cached: HashMap<TypeId, JsValue>,
    /// What reads a `Blob`'s bytes for `b.bytes({ from: ["blob"] })`.
    pub(crate) blob_reader: Option<crate::bindgen::BlobReader>,
}

/// What an uncatchable error is seen as from Rust: Boa has unwound every frame of JavaScript, as JavaScriptCore does
/// for a stack overflow, and the value is the `RangeError` JavaScriptCore throws then.
fn uncatchable_as_value(error: &boa_engine::JsError, context: &mut Context) -> JsValue {
    let message = match error.as_engine() {
        Some(EngineError::RuntimeLimit(
            RuntimeLimitError::Recursion | RuntimeLimitError::StackSize,
        )) => "Maximum call stack size exceeded.".to_owned(),
        Some(EngineError::Panic(panic)) if panic.message() == TERMINATED => {
            return JsNativeError::error()
                .with_message(TERMINATED)
                .into_opaque(context)
                .into();
        }
        Some(engine) => engine.to_string(),
        None => unreachable!("only an engine error is uncatchable"),
    };
    JsNativeError::range()
        .with_message(message)
        .into_opaque(context)
        .into()
}

/// The message of the uncatchable error that `JsError::Terminated` is in Boa.
const TERMINATED: &str = "the JavaScript thread was terminated";

impl JSGlobalObject {
    /// The global object of a context Boa hands to a native function.
    pub fn from_context(context: &mut Context) -> &JSGlobalObject {
        // SAFETY: `JSGlobalObject` is `repr(transparent)` over `UnsafeCell<Context>`, which has `Context`'s layout,
        // and the exclusive borrow is what every later `context()` derives from.
        unsafe { &*(core::ptr::from_mut(context) as *const JSGlobalObject) }
    }

    /// Runs `f` in a new realm with Boa's built-ins, whose modules are the native ones.
    pub fn with_new<R>(f: impl FnOnce(&JSGlobalObject) -> R) -> R {
        let loader = Loader::new(Rc::new(IdleModuleLoader));
        let mut context = Context::builder()
            .module_loader(Rc::new(loader))
            .job_executor(Rc::new(crate::AsyncContextJobExecutor::default()))
            .build()
            .expect("a context with Boa's defaults builds");
        f(Self::from_context(&mut context))
    }

    /// Boa's context.
    ///
    /// # Safety
    /// The reference is the only one in use until it is dropped: nothing reaches this global object meanwhile,
    /// except Boa through it (a native function that Boa calls gets a reborrow of it).
    #[allow(clippy::mut_from_ref)]
    pub unsafe fn context(&self) -> &mut Context {
        // SAFETY: the caller's contract.
        unsafe { &mut *self.0.get() }
    }

    /// Every function of this crate uses the context for one engine operation and drops it before it calls
    /// anything else that takes `self`.
    #[allow(clippy::mut_from_ref)]
    pub(crate) fn cx(&self) -> &mut Context {
        // SAFETY: see above; a native function Boa enters meanwhile derives its own from Boa's reborrow.
        unsafe { self.context() }
    }

    pub(crate) fn state(&self) -> &mut State {
        let data = self.cx().host_defined_mut();
        if data.get::<State>().is_none() {
            data.insert(State::default());
        }
        data.get_mut::<State>().expect("inserted above")
    }

    // ───────────────────────────── exceptions ─────────────────────────────

    /// Makes a failed Boa operation the pending exception: for an embedder that calls Boa (`engine`) directly.
    pub fn throw_engine_error(&self, error: boa_engine::JsError) -> JsError {
        self.state().exception = Some(error);
        JsError::Thrown
    }

    /// Lifts a Boa result into a `JsResult`: for an embedder that calls Boa for what this crate does not wrap.
    pub fn lift<T>(&self, result: boa_engine::JsResult<T>) -> JsResult<T> {
        result.map_err(|error| self.throw_engine_error(error))
    }

    /// The Boa error a native function returns for `error`, which clears the pending exception: for an embedder
    /// that calls `bun_js` from a Boa native function of its own.
    pub fn into_engine_error(&self, error: JsError) -> boa_engine::JsError {
        match error {
            JsError::Thrown => self.state().exception.take().unwrap_or_else(|| {
                debug_assert!(false, "JsError::Thrown with no exception pending");
                JsNativeError::error()
                    .with_message("a native function failed without an exception")
                    .into()
            }),
            JsError::OutOfMemory => JsNativeError::range().with_message("Out of memory").into(),
            // Script cannot catch it: every frame unwinds.
            JsError::Terminated => EngineError::Panic(PanicError::new(TERMINATED)).into(),
        }
    }

    pub fn throw_value(&self, value: JSValue) -> JsError {
        self.throw_engine_error(boa_engine::JsError::from_opaque(value.0))
    }

    pub fn throw(&self, args: fmt::Arguments<'_>) -> JsError {
        self.throw_engine_error(JsNativeError::error().with_message(args.to_string()).into())
    }

    pub fn throw_type_error(&self, args: fmt::Arguments<'_>) -> JsError {
        self.throw_engine_error(JsNativeError::typ().with_message(args.to_string()).into())
    }

    pub fn throw_range_error(&self, args: fmt::Arguments<'_>) -> JsError {
        self.throw_engine_error(JsNativeError::range().with_message(args.to_string()).into())
    }

    pub fn throw_syntax_error(&self, args: fmt::Arguments<'_>) -> JsError {
        self.throw_engine_error(
            JsNativeError::syntax()
                .with_message(args.to_string())
                .into(),
        )
    }

    pub fn throw_out_of_memory(&self) -> JsError {
        let error = self.into_engine_error(JsError::OutOfMemory);
        self.throw_engine_error(error)
    }

    fn create(&self, error: JsNativeError) -> JSValue {
        JSValue(error.into_opaque(self.cx()).into())
    }

    pub(crate) fn create_error_of_kind(
        &self,
        kind: ErrorKind,
        args: fmt::Arguments<'_>,
    ) -> JSValue {
        let error = match kind {
            ErrorKind::Error => JsNativeError::error(),
            ErrorKind::TypeError => JsNativeError::typ(),
            ErrorKind::RangeError => JsNativeError::range(),
            ErrorKind::SyntaxError => JsNativeError::syntax(),
            ErrorKind::URIError => JsNativeError::uri(),
        };
        self.create(error.with_message(args.to_string()))
    }

    pub fn create_error_instance(&self, args: fmt::Arguments<'_>) -> JSValue {
        self.create(JsNativeError::error().with_message(args.to_string()))
    }

    pub fn create_type_error_instance(&self, args: fmt::Arguments<'_>) -> JSValue {
        self.create(JsNativeError::typ().with_message(args.to_string()))
    }

    pub fn create_range_error_instance(&self, args: fmt::Arguments<'_>) -> JSValue {
        self.create(JsNativeError::range().with_message(args.to_string()))
    }

    pub fn create_syntax_error_instance(&self, args: fmt::Arguments<'_>) -> JSValue {
        self.create(JsNativeError::syntax().with_message(args.to_string()))
    }

    pub fn has_exception(&self) -> bool {
        self.state().exception.is_some()
    }

    pub fn clear_exception(&self) {
        self.state().exception = None;
    }

    /// Clears the pending exception unless script could not catch it (a runtime limit, a termination); whether it
    /// did.
    pub fn clear_catchable_exception(&self) -> bool {
        let state = self.state();
        if state
            .exception
            .as_ref()
            .is_some_and(|error| error.as_engine().is_none())
        {
            state.exception = None;
            return true;
        }
        false
    }

    /// The pending exception as a value, which clears it. Rust sees an uncatchable one (a runtime limit, a
    /// termination) as an error value too; script never does.
    pub fn try_take_exception(&self) -> Option<JSValue> {
        let error = self.state().exception.take()?;
        Some(JSValue(self.engine_error_as_value(error)))
    }

    /// The pending exception as a value, which stays pending.
    pub fn peek_exception(&self) -> Option<JSValue> {
        let error = self.state().exception.clone()?;
        Some(JSValue(self.engine_error_as_value(error)))
    }

    fn engine_error_as_value(&self, error: boa_engine::JsError) -> JsValue {
        if error.as_engine().is_some() {
            return uncatchable_as_value(&error, self.cx());
        }
        error
            .into_opaque(self.cx())
            .expect("a catchable error is a value")
    }

    /// The exception that `proof` says is pending, which clears it.
    pub fn take_exception(&self, proof: JsError) -> JSValue {
        if proof != JsError::Thrown {
            let error = self.into_engine_error(proof);
            self.throw_engine_error(error);
        }
        self.try_take_exception().expect(
            "A JavaScript exception was thrown, but it was cleared before it could be read.",
        )
    }

    // ───────────────────────────── the realm ─────────────────────────────

    /// The value `K` names in this realm, made by `make` the first time: what a `LazyProperty` of JavaScriptCore's
    /// global object is. A `make` that throws makes nothing, and the next call tries again.
    pub fn cached<K: 'static>(
        &self,
        make: impl FnOnce(&JSGlobalObject) -> JsResult<JSValue>,
    ) -> JsResult<JSValue> {
        if let Some(value) = self.state().cached.get(&TypeId::of::<K>()) {
            return Ok(JSValue(value.clone()));
        }
        let value = make(self)?;
        self.state()
            .cached
            .insert(TypeId::of::<K>(), value.0.clone());
        Ok(value)
    }

    /// `globalThis`.
    pub fn to_js_value(&self) -> JSValue {
        JSValue(self.cx().global_object().into())
    }

    /// Evaluates a script, for an embedder or a test.
    pub fn evaluate_script(&self, source: &str) -> JsResult<JSValue> {
        let result = self.cx().eval(Source::from_bytes(source));
        self.lift(result).map(JSValue)
    }

    /// Runs the jobs that are queued: promise reactions, then what they queue.
    pub fn drain_microtasks(&self) -> JsResult<()> {
        let result = self.cx().run_jobs();
        self.lift(result)
    }
}
