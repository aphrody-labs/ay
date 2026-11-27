//! What the Rust that `src/codegen/bindgen-js.ts` writes from a `.bind.ts` file calls, and what the bodies it calls
//! take: the same names as `bun_jsc::bindgen` (`JsHeap`, `JsScope`, `Error`, `Result`, `JsLocal`, `to_js::*`,
//! `host_fn_return`), so that a body compiles against either. JavaScriptCore's thunks have C++ convert the
//! arguments; here `from_js` does, in Rust.

use core::cell::RefCell;
use core::fmt;
use core::marker::PhantomData;
use core::ptr::NonNull;

use crate::{ErrorCode, JSGlobalObject, JSValue, JsError, JsResult};

mod bytes;
mod wrapper;

pub use bytes::{BlobBytes, BlobReader, BufferSource, JsBytes, JsStr, StringOrJsBytes};
pub use wrapper::{IntoInstance, JsPayload, JsWrapper};

/// `'a` is the call: nothing a body is handed outlives it.
type Invariant<'a> = PhantomData<fn(&'a ()) -> &'a ()>;

/// The bytes a conversion made for the call, freed when it returns.
#[derive(Default)]
struct Arena(RefCell<Vec<NonNull<[u8]>>>);

impl Arena {
    fn keep(&self, bytes: Vec<u8>) -> NonNull<[u8]> {
        let bytes = NonNull::from(Box::leak(bytes.into_boxed_slice()));
        self.0.borrow_mut().push(bytes);
        bytes
    }
}

impl Drop for Arena {
    fn drop(&mut self) {
        for bytes in self.0.get_mut().drain(..) {
            // SAFETY: each came from `Box::leak` in `keep`, and the call that borrowed it has returned.
            drop(unsafe { Box::from_raw(bytes.as_ptr()) });
        }
    }
}

/// What allocates JavaScript values for the call.
pub struct JsHeap<'a> {
    global: &'a JSGlobalObject,
    arena: &'a Arena,
    _call: Invariant<'a>,
}

/// What runs JavaScript for the call (`capability: "js"`).
pub struct JsScope<'a> {
    pub heap: JsHeap<'a>,
}

impl JsScope<'_> {
    /// Runs `f` as a call whose `'a` it cannot name.
    pub fn with<R>(global: &JSGlobalObject, f: impl for<'a> FnOnce(&mut JsScope<'a>) -> R) -> R {
        let arena = Arena::default();
        let mut scope = JsScope {
            heap: JsHeap {
                global,
                arena: &arena,
                _call: PhantomData,
            },
        };
        f(&mut scope)
    }

    pub fn global(&mut self) -> &JSGlobalObject {
        self.heap.global
    }
}

impl<'a> JsHeap<'a> {
    pub fn global_object(&self) -> &'a JSGlobalObject {
        self.global
    }

    /// Bytes that live as long as the call.
    pub fn keep(&self, bytes: Vec<u8>) -> &'a [u8] {
        // SAFETY: the arena frees them when the call returns, which `'a` cannot outlive.
        unsafe { self.arena.keep(bytes).as_ref() }
    }

    pub fn check_termination(&self) -> Result<'a, ()> {
        Ok(())
    }

    pub fn create_utf8_string(&self, utf8: &[u8]) -> Result<'a, JsLocal<'a>> {
        Ok(JsLocal::from_raw(JSValue::from_utf8(utf8)))
    }

    pub fn create_empty_object(&self) -> JsLocal<'a> {
        JsLocal::from_raw(JSValue::create_empty_object(self.global, 0))
    }

    pub fn create_array_of(&self, values: &[JsLocal<'a>]) -> Result<'a, JsLocal<'a>> {
        let values: Vec<JSValue> = values.iter().map(|value| value.0.clone()).collect();
        let array = JSValue::create_array_from_slice(self.global, &values)?;
        Ok(JsLocal::from_raw(array))
    }

    pub fn error(&self, args: fmt::Arguments<'_>) -> Error<'a> {
        Error::unthrown(self.global.create_error_instance(args))
    }

    pub fn type_error(&self, args: fmt::Arguments<'_>) -> Error<'a> {
        Error::unthrown(self.global.create_type_error_instance(args))
    }

    pub fn range_error(&self, args: fmt::Arguments<'_>) -> Error<'a> {
        Error::unthrown(self.global.create_range_error_instance(args))
    }

    pub fn syntax_error(&self, args: fmt::Arguments<'_>) -> Error<'a> {
        Error::unthrown(self.global.create_syntax_error_instance(args))
    }

    /// As [`error`](Self::error), with a `code`, which also picks the class.
    pub fn error_with_code(&self, code: ErrorCode, args: fmt::Arguments<'_>) -> Error<'a> {
        Error::unthrown(code.fmt(self.global, args))
    }

    pub fn invalid_arguments(&self, args: fmt::Arguments<'_>) -> Error<'a> {
        self.error_with_code(ErrorCode::INVALID_ARG_TYPE, args)
    }

    pub fn memory_allocation_failed(&self) -> Error<'a> {
        Error::pending(JsError::OutOfMemory)
    }
}

// ───────────────────────────── errors ─────────────────────────────

enum ErrorState {
    /// Thrown already: the exception is pending on the global object.
    Pending(JsError),
    /// To throw once the body has returned.
    ToThrow(JSValue),
}

/// What a body fails with.
pub struct Error<'a>(ErrorState, Invariant<'a>);
pub type Result<'a, T> = core::result::Result<T, Error<'a>>;

impl<'a> Error<'a> {
    fn unthrown(value: JSValue) -> Self {
        Error(ErrorState::ToThrow(value), PhantomData)
    }

    fn pending(error: JsError) -> Self {
        Error(ErrorState::Pending(error), PhantomData)
    }

    /// `throw value`, once the body has returned.
    pub fn from_value<T>(value: JsLocal<'a, T>) -> Self {
        Self::unthrown(value.0)
    }

    #[doc(hidden)]
    pub fn into_host_fn_return<T>(self, cx: &mut JsScope<'a>) -> JsResult<T> {
        Err(self.into_js_error(cx.global()))
    }

    /// Throws it now, for a caller that is not generated.
    pub fn into_js_error(self, global: &JSGlobalObject) -> JsError {
        match self.0 {
            ErrorState::Pending(error) => error,
            ErrorState::ToThrow(value) => global.throw_value(value),
        }
    }
}

impl From<JsError> for Error<'_> {
    fn from(error: JsError) -> Self {
        Error::pending(error)
    }
}

impl From<bun_alloc::AllocError> for Error<'_> {
    fn from(_: bun_alloc::AllocError) -> Self {
        Error::pending(JsError::OutOfMemory)
    }
}

/// An error type a body may return instead of [`Error`]. The generated code converts it.
pub trait IntoError<'a> {
    fn into_error(self, heap: &JsHeap<'a>) -> Error<'a>;
}

impl<'a> IntoError<'a> for Error<'a> {
    fn into_error(self, _: &JsHeap<'a>) -> Error<'a> {
        self
    }
}

impl<'a> IntoError<'a> for JsError {
    fn into_error(self, _: &JsHeap<'a>) -> Error<'a> {
        Error::pending(self)
    }
}

impl<'a> IntoError<'a> for bun_alloc::AllocError {
    fn into_error(self, _: &JsHeap<'a>) -> Error<'a> {
        Error::pending(JsError::OutOfMemory)
    }
}

impl<'a, E: IntoError<'a>> IntoError<'a> for Box<E> {
    fn into_error(self, heap: &JsHeap<'a>) -> Error<'a> {
        (*self).into_error(heap)
    }
}

impl<'a> IntoError<'a> for core::convert::Infallible {
    fn into_error(self, _: &JsHeap<'a>) -> Error<'a> {
        match self {}
    }
}

// ───────────────────────────── values ─────────────────────────────

/// A value of the call, known to be a `T`.
pub struct JsLocal<'a, T = JSValue>(JSValue, PhantomData<(Invariant<'a>, *const T)>);

impl<T> Clone for JsLocal<'_, T> {
    fn clone(&self) -> Self {
        JsLocal(self.0.clone(), PhantomData)
    }
}

impl<'a, T> JsLocal<'a, T> {
    fn from_raw(value: JSValue) -> Self {
        JsLocal(value, PhantomData)
    }

    pub fn to_js(self) -> JSValue {
        self.0
    }

    pub fn as_value(self) -> JsLocal<'a> {
        JsLocal::from_raw(self.0)
    }

    pub fn value(&self) -> &JSValue {
        &self.0
    }
}

impl<'a> JsLocal<'a> {
    pub const UNDEFINED: Self = JsLocal(JSValue::UNDEFINED, PhantomData);

    pub fn new(_: &JsHeap<'a>, value: JSValue) -> Self {
        JsLocal::from_raw(value)
    }

    pub fn is_undefined(&self) -> bool {
        self.0.is_undefined()
    }
}

/// Converting results.
pub mod to_js {
    use super::*;

    macro_rules! number {
        ($($name:ident: $t:ty => $f:expr;)*) => {$(
            pub fn $name<'a>(_: &JsHeap<'a>, value: $t) -> Result<'a, JsLocal<'a>> {
                Ok(JsLocal::from_raw($f(value)))
            }
        )*};
    }
    number! {
        bool: bool => JSValue::js_boolean;
        u8: u8 => |n| JSValue::js_number_from_int32(i32::from(n));
        u16: u16 => |n| JSValue::js_number_from_int32(i32::from(n));
        i32: i32 => JSValue::js_number_from_int32;
        u32: u32 => |n| JSValue::js_number(f64::from(n));
        u64: u64 => JSValue::js_number_from_uint64;
        usize: usize => |n| JSValue::js_number_from_uint64(n as u64);
        f64: f64 => JSValue::js_number;
    }

    pub fn undefined<'a>(_: &JsHeap<'a>, (): ()) -> Result<'a, JsLocal<'a>> {
        Ok(JsLocal::from_raw(JSValue::UNDEFINED))
    }

    /// A body may also return a plain [`JSValue`].
    pub trait Any<'a> {
        fn into_value(self, heap: &JsHeap<'a>) -> JsLocal<'a>;
    }

    impl<'a, T> Any<'a> for JsLocal<'a, T> {
        fn into_value(self, _: &JsHeap<'a>) -> JsLocal<'a> {
            self.as_value()
        }
    }

    impl<'a> Any<'a> for JSValue {
        fn into_value(self, _: &JsHeap<'a>) -> JsLocal<'a> {
            JsLocal::from_raw(self)
        }
    }

    pub fn any<'a>(heap: &JsHeap<'a>, value: impl Any<'a>) -> Result<'a, JsLocal<'a>> {
        Ok(value.into_value(heap))
    }

    pub fn utf8<'a>(heap: &JsHeap<'a>, value: impl AsRef<[u8]>) -> Result<'a, JsLocal<'a>> {
        heap.create_utf8_string(value.as_ref())
    }

    /// A new instance of a class whose payload is `value`, as JavaScriptCore's `into_js` of a `JsPayload`, or the
    /// instance a `JsWrapper` is.
    pub fn class<'a, T: crate::JsClass>(
        heap: &JsHeap<'a>,
        value: impl super::IntoInstance<'a, T>,
    ) -> Result<'a, JsLocal<'a>> {
        Ok(JsLocal::from_raw(value.into_instance(heap)?))
    }

    /// `b.u64.asBigInt`.
    pub fn u64_as_bigint<'a>(_: &JsHeap<'a>, value: u64) -> Result<'a, JsLocal<'a>> {
        Ok(JsLocal::from_raw(JSValue(boa_engine::JsBigInt::from(value).into())))
    }

    /// `b.array(T)`: a new array of each element, converted by `convert`.
    pub fn array<'a, T>(
        heap: &JsHeap<'a>,
        values: impl IntoIterator<Item = T>,
        convert: impl Fn(&JsHeap<'a>, T) -> Result<'a, JsLocal<'a>>,
    ) -> Result<'a, JsLocal<'a>> {
        let values = values
            .into_iter()
            .map(|value| convert(heap, value))
            .collect::<Result<'a, Vec<_>>>()?;
        heap.create_array_of(&values)
    }
}

/// Converting arguments, as C++'s conversion contexts do (see [`Context`]): a value of the wrong type throws
/// `ERR_INVALID_ARG_TYPE` (`a must be a number`), a number out of range `ERR_OUT_OF_RANGE`
/// (`a must be an integer (received 1.5)`, `a must be in the range [0, 255] (received 256)`).
pub mod from_js {
    use super::*;

    /// What the error of a conversion says, as C++'s conversion contexts.
    #[derive(Clone, Copy, Debug)]
    pub enum Context<'n> {
        /// `LiteralConversionContext`: `a must be a number`.
        Argument(&'n str),
        /// `ValidatorArgumentContext`, for an argument that `validatorExpects` names: what
        /// src/runtime/node/util/validators.rs throws, `The "a" property must be of type string, got number`. `index`
        /// is that of an element of a `b.rest`: `paths[1]`.
        Validator {
            name: &'n str,
            expected: &'n str,
            index: Option<usize>,
        },
        /// `NodeArgumentContext`, for an argument with a list of `expected`: Node's
        /// `ERR_INVALID_ARG_TYPE(name, expected, actual)`, `The "a" argument must be of type string. Received …`.
        Node { name: &'n str, expected: &'n [&'n str] },
        /// `ArgumentMessageContext`, for an argument whose `expected` is a string: the whole message.
        Message { name: &'n str, message: &'n str },
    }

    impl Context<'_> {
        pub fn name(&self) -> &str {
            match self {
                Context::Argument(name)
                | Context::Validator { name, .. }
                | Context::Node { name, .. }
                | Context::Message { name, .. } => name,
            }
        }
    }

    /// Node's `ERR_INVALID_ARG_TYPE(name, expected, actual)` message (ErrorCode.cpp's): the expected types grouped
    /// into type names, class names and the rest.
    pub fn node_invalid_arg_type_message(
        global: &JSGlobalObject,
        name: &str,
        expected: &[&str],
        value: &JSValue,
    ) -> JsResult<String> {
        fn list(items: &[String]) -> String {
            match items {
                [] => String::new(),
                [one] => one.clone(),
                [a, b] => format!("{a} or {b}"),
                [a, b, c] => format!("{a}, {b}, or {c}"),
                [rest @ .., last] => format!("{}, or {last}", rest.join(", ")),
            }
        }
        const PRIMITIVES: &[&str] =
            &["string", "function", "number", "object", "Function", "Object", "boolean", "bigint", "symbol"];
        let is_class = |name: &str| {
            let mut chars = name.chars();
            chars.next().is_some_and(|c| c.is_ascii_uppercase()) && chars.all(|c| c.is_ascii_alphanumeric())
        };
        let (mut types, mut instances, mut other) = (Vec::new(), Vec::new(), Vec::new());
        for &kind in expected {
            if PRIMITIVES.contains(&kind) {
                types.push(kind.to_ascii_lowercase());
            } else if is_class(kind) {
                instances.push(kind.to_owned());
            } else {
                other.push(kind.to_owned());
            }
        }
        if !instances.is_empty() {
            if let Some(at) = types.iter().position(|kind| kind == "object") {
                types.remove(at);
                instances.push("Object".to_owned());
            }
        }
        let mut message = String::from("The ");
        if name.ends_with(" argument") {
            message.push_str(name);
        } else {
            let noun = if name.contains('.') { "property" } else { "argument" };
            message.push_str(&format!("\"{name}\" {noun}"));
        }
        message.push_str(" must be ");
        if !types.is_empty() {
            message.push_str(if types.len() > 1 { "one of type " } else { "of type " });
            message.push_str(&list(&types));
            if !instances.is_empty() || !other.is_empty() {
                message.push_str(" or ");
            }
        }
        if !instances.is_empty() {
            message.push_str("an instance of ");
            message.push_str(&list(&instances));
            if !other.is_empty() {
                message.push_str(" or ");
            }
        }
        match other.as_slice() {
            [] => {}
            [one] => {
                if one.to_ascii_lowercase() != *one {
                    message.push_str("an ");
                }
                message.push_str(one);
            }
            many => {
                message.push_str("one of ");
                message.push_str(&list(many));
            }
        }
        message.push_str(". Received ");
        message.push_str(&determine_specific_type(global, value)?);
        Ok(message)
    }

    fn node_invalid_arg_type<'a>(heap: &JsHeap<'a>, name: &str, expected: &[&str], value: &JSValue) -> Error<'a> {
        match node_invalid_arg_type_message(heap.global_object(), name, expected, value) {
            Ok(message) => heap.error_with_code(ErrorCode::INVALID_ARG_TYPE, format_args!("{message}")),
            Err(error) => Error::pending(error),
        }
    }

    /// `ERR_INVALID_ARG_TYPE` as Node's validators (src/runtime/node/util/validators.rs) throw it:
    /// `The "name" property must be of type string, got number`, where an array is `array`.
    pub fn invalid_arg_type<'a>(
        heap: &JsHeap<'a>,
        name: impl fmt::Display,
        expected: &str,
        value: &JSValue,
    ) -> Error<'a> {
        let actual = value.type_name(heap.global_object());
        heap.error_with_code(
            ErrorCode::INVALID_ARG_TYPE,
            format_args!("The \"{name}\" property must be of type {expected}, got {actual}"),
        )
    }

    fn type_must_be<'a>(
        heap: &JsHeap<'a>,
        ctx: Context<'_>,
        noun_phrase: &str,
        value: &JSValue,
    ) -> Error<'a> {
        match ctx {
            Context::Argument(name) => heap.error_with_code(
                ErrorCode::INVALID_ARG_TYPE,
                format_args!("{name} must be {noun_phrase}"),
            ),
            Context::Validator {
                name,
                expected,
                index: None,
            } => invalid_arg_type(heap, name, expected, value),
            Context::Validator {
                name,
                expected,
                index: Some(index),
            } => invalid_arg_type(heap, format_args!("{name}[{index}]"), expected, value),
            Context::Node { name, expected } => node_invalid_arg_type(heap, name, expected, value),
            Context::Message { message, .. } => {
                heap.error_with_code(ErrorCode::INVALID_ARG_TYPE, format_args!("{message}"))
            }
        }
    }

    /// What `b.bytes` takes without `ToString`, and what it refuses: C++'s `BytesOptions`.
    #[derive(Clone, Copy, Debug, Default)]
    pub struct BytesFrom {
        pub string: bool,
        pub buffer: bool,
        pub blob: bool,
        pub to_string_otherwise: bool,
        pub refuse_undefined: bool,
        pub refuse_null: bool,
        pub refuse_string_object: bool,
    }

    /// What a `b.bytes` argument was.
    enum Taken {
        /// A string, or what `ToString` made one.
        String(JSValue),
        Buffer(JSValue),
        Blob(Vec<u8>),
    }

    /// C++'s `BytesConverter::convertInto`. `expected` is what the error says it takes: `a string, ArrayBuffer,
    /// ArrayBufferView or Blob`.
    fn take_bytes<'a>(
        heap: &JsHeap<'a>,
        value: &JSValue,
        ctx: Context<'_>,
        from: BytesFrom,
        expected: &str,
    ) -> Result<'a, Taken> {
        let global = heap.global_object();
        if from.string && value.is_string() {
            return Ok(Taken::String(value.clone()));
        }
        if from.buffer && (value.is_array_buffer_view() || value.is_array_buffer()) {
            return Ok(Taken::Buffer(value.clone()));
        }
        if from.blob {
            match lift(global.blob_bytes(value))? {
                BlobBytes::InMemory(bytes) => return Ok(Taken::Blob(bytes)),
                BlobBytes::NotInMemory => {
                    return Err(heap.error_with_code(
                        ErrorCode::INVALID_ARG_TYPE,
                        format_args!("File blob cannot be used here"),
                    ));
                }
                BlobBytes::NotBlob => {}
            }
        }
        let refused = if from.to_string_otherwise {
            (from.refuse_undefined && value.is_undefined()) || (from.refuse_null && value.is_null())
        } else {
            !from.string || from.refuse_string_object || !value.is_string_object()
        };
        if refused {
            return Err(type_must_be(heap, ctx, expected, value));
        }
        Ok(Taken::String(lift(value.to_js_string(global))?))
    }

    fn utf8_of(value: &JSValue) -> Vec<u8> {
        JsStr::new(value.clone()).to_utf8()
    }

    /// `b.bytes`, `held: "borrowed"`: the bytes, which live as long as the call.
    pub fn bytes<'a>(
        heap: &JsHeap<'a>,
        value: &JSValue,
        ctx: Context<'_>,
        from: BytesFrom,
        expected: &str,
    ) -> Result<'a, &'a [u8]> {
        Ok(heap.keep(copied_bytes(heap, value, ctx, from, expected)?))
    }

    /// `b.bytes`, `held: "copied"`: the body's own copy.
    pub fn copied_bytes<'a>(
        heap: &JsHeap<'a>,
        value: &JSValue,
        ctx: Context<'_>,
        from: BytesFrom,
        expected: &str,
    ) -> Result<'a, Vec<u8>> {
        Ok(match take_bytes(heap, value, ctx, from, expected)? {
            Taken::String(string) => utf8_of(&string),
            Taken::Buffer(buffer) => super::bytes::read_bytes(heap.global_object(), &buffer),
            Taken::Blob(bytes) => bytes,
        })
    }

    /// `b.bytes`, `held: "handle"`: a string as its UTF-8, a buffer as itself.
    pub fn bytes_handle<'a>(
        heap: &JsHeap<'a>,
        value: &JSValue,
        ctx: Context<'_>,
        from: BytesFrom,
        expected: &str,
    ) -> Result<'a, JsBytes<'a>> {
        Ok(match take_bytes(heap, value, ctx, from, expected)? {
            Taken::String(string) => JsBytes::kept(heap.keep(utf8_of(&string))),
            Taken::Buffer(buffer) => JsBytes::of_buffer(buffer),
            Taken::Blob(bytes) => JsBytes::kept(heap.keep(bytes)),
        })
    }

    /// `b.bytes`, `string: "asIs", held: "handle"`.
    pub fn string_or_bytes<'a>(
        heap: &JsHeap<'a>,
        value: &JSValue,
        ctx: Context<'_>,
        from: BytesFrom,
        expected: &str,
    ) -> Result<'a, StringOrJsBytes<'a>> {
        Ok(match take_bytes(heap, value, ctx, from, expected)? {
            Taken::String(string) => StringOrJsBytes::String(JsStr::new(string)),
            Taken::Buffer(buffer) => StringOrJsBytes::Bytes(JsBytes::of_buffer(buffer)),
            Taken::Blob(bytes) => StringOrJsBytes::Bytes(JsBytes::kept(heap.keep(bytes))),
        })
    }

    /// `b.bufferSource`: a typed array, `DataView` or `ArrayBuffer`; a shared one only with `allow_shared`.
    pub fn buffer_source<'a>(
        heap: &JsHeap<'a>,
        value: &JSValue,
        ctx: Context<'_>,
        allow_shared: bool,
    ) -> Result<'a, JsLocal<'a, BufferSource>> {
        if !value.is_array_buffer_view() && !value.is_array_buffer() {
            return Err(type_must_be(heap, ctx, "an ArrayBuffer or ArrayBufferView", value));
        }
        if !allow_shared && value.is_shared_buffer_source(heap.global_object()) {
            return Err(type_must_be(heap, ctx, "an ArrayBuffer or ArrayBufferView that is not shared", value));
        }
        Ok(JsLocal::from_raw(value.clone()))
    }

    /// `b.string`: a string (or a `String` object, which `ToString` makes one).
    pub fn string<'a>(heap: &JsHeap<'a>, value: &JSValue, ctx: Context<'_>) -> Result<'a, JsStr<'a>> {
        if value.is_string() {
            return Ok(JsStr::new(value.clone()));
        }
        if !value.is_string_object() {
            return Err(type_must_be(heap, ctx, "a string", value));
        }
        Ok(JsStr::new(lift(value.to_js_string(heap.global_object()))?))
    }

    /// `b.string.loose`: `ToString`.
    pub fn loose_string<'a>(heap: &JsHeap<'a>, value: &JSValue, _: Context<'_>) -> Result<'a, JsStr<'a>> {
        Ok(JsStr::new(lift(value.to_js_string(heap.global_object()))?))
    }

    /// `b.callable`: a function, as itself.
    pub fn callable<'a>(heap: &JsHeap<'a>, value: &JSValue, ctx: Context<'_>) -> Result<'a, JsLocal<'a>> {
        if !value.is_callable() {
            return Err(type_must_be(heap, ctx, "a function", value));
        }
        Ok(JsLocal::new(heap, value.clone()))
    }

    /// `b.f64.allowDate`: a number, or a `Date`'s time.
    pub fn strict_f64_or_date<'a>(heap: &JsHeap<'a>, value: &JSValue, ctx: Context<'_>) -> Result<'a, f64> {
        let global = heap.global_object();
        let date = value.0.as_object().and_then(|object| boa_engine::object::builtins::JsDate::from_object(object).ok());
        if let Some(date) = date {
            let time = date.get_time(global.cx());
            return Ok(lift(global.lift(time))?.as_number().unwrap_or(f64::NAN));
        }
        strict_f64(heap, value, ctx)
    }

    fn range<'a>(heap: &JsHeap<'a>, ctx: Context<'_>, predicate: fmt::Arguments<'_>) -> Error<'a> {
        let name = ctx.name();
        heap.error_with_code(ErrorCode::OUT_OF_RANGE, format_args!("{name} {predicate}"))
    }

    /// A number as JavaScript prints it, as WTF's `makeString` does a double.
    fn number_text(heap: &JsHeap<'_>, number: f64) -> String {
        JSValue::js_number(number)
            .to_utf8(heap.global_object())
            .unwrap_or_default()
    }

    /// `ERR_MISSING_ARGS`, with the message the generator wrote: `The "a" and "b" arguments must be specified`.
    pub fn missing_arguments<'a>(heap: &JsHeap<'a>, message: &str) -> Error<'a> {
        heap.error_with_code(ErrorCode::MISSING_ARGS, format_args!("{message}"))
    }

    fn lift<'a, T>(result: JsResult<T>) -> Result<'a, T> {
        result.map_err(Error::pending)
    }

    pub fn any<'a>(heap: &JsHeap<'a>, value: &JSValue, _: Context<'_>) -> Result<'a, JsLocal<'a>> {
        Ok(JsLocal::new(heap, value.clone()))
    }

    pub fn strict_bool<'a>(heap: &JsHeap<'a>, value: &JSValue, ctx: Context<'_>) -> Result<'a, bool> {
        value
            .as_engine()
            .as_boolean()
            .ok_or_else(|| type_must_be(heap, ctx, "a boolean", value))
    }

    /// `ToBoolean`.
    pub fn loose_bool<'a>(_: &JsHeap<'a>, value: &JSValue, _: Context<'_>) -> Result<'a, bool> {
        Ok(value.to_boolean())
    }

    pub fn strict_f64<'a>(heap: &JsHeap<'a>, value: &JSValue, ctx: Context<'_>) -> Result<'a, f64> {
        value
            .get_number()
            .ok_or_else(|| type_must_be(heap, ctx, "a number", value))
    }

    /// `ToNumber`.
    pub fn loose_f64<'a>(heap: &JsHeap<'a>, value: &JSValue, _: Context<'_>) -> Result<'a, f64> {
        lift(value.to_number(heap.global_object()))
    }

    /// An integer of `T`: a number that is a safe integer, and in `T`'s range.
    pub fn strict_integer<'a, T: Integer>(
        heap: &JsHeap<'a>,
        value: &JSValue,
        ctx: Context<'_>,
    ) -> Result<'a, T> {
        let number = value
            .get_number()
            .ok_or_else(|| type_must_be(heap, ctx, "a number", value))?;
        integer_of_number(heap, number, ctx)
    }

    /// `.allowBigInt`: a BigInt in the range of `T`, or what [`strict_integer`] takes.
    pub fn strict_integer_or_bigint<'a, T: Integer + TryFrom<i128>>(
        heap: &JsHeap<'a>,
        value: &JSValue,
        ctx: Context<'_>,
    ) -> Result<'a, T> {
        let Some(bigint) = value.as_engine().as_bigint() else {
            return strict_integer(heap, value, ctx);
        };
        let integer = i128::try_from(bigint.as_inner()).ok();
        integer.and_then(|integer| T::try_from(integer).ok()).ok_or_else(|| {
            range(
                heap,
                ctx,
                format_args!(
                    "must be in the range [{}, {}] (received {}n)",
                    T::MIN,
                    T::MAX,
                    bigint.to_string_radix(10)
                ),
            )
        })
    }

    /// `ToNumber`, then as [`strict_integer`]: 1.5, NaN and Infinity are out of range, not truncated.
    pub fn loose_integer<'a, T: Integer>(
        heap: &JsHeap<'a>,
        value: &JSValue,
        ctx: Context<'_>,
    ) -> Result<'a, T> {
        let number = lift(value.to_number(heap.global_object()))?;
        integer_of_number(heap, number, ctx)
    }

    fn integer_of_number<'a, T: Integer>(
        heap: &JsHeap<'a>,
        number: f64,
        ctx: Context<'_>,
    ) -> Result<'a, T> {
        const MAX_SAFE_INTEGER: f64 = 9_007_199_254_740_991.0;
        if !(number.trunc() == number && number.abs() <= MAX_SAFE_INTEGER) {
            let received = number_text(heap, number);
            return Err(range(
                heap,
                ctx,
                format_args!("must be an integer (received {received})"),
            ));
        }
        let integer = number as i64;
        T::try_from(integer).map_err(|_| {
            range(
                heap,
                ctx,
                format_args!(
                    "must be in the range [{}, {}] (received {integer})",
                    T::MIN,
                    T::MAX
                ),
            )
        })
    }

    /// A string (or a `String` object), as UTF-8 for the call.
    pub fn strict_utf8<'a>(
        heap: &JsHeap<'a>,
        value: &JSValue,
        ctx: Context<'_>,
    ) -> Result<'a, &'a [u8]> {
        if !value.is_string() && !value.is_string_object() {
            return Err(type_must_be(heap, ctx, "a string", value));
        }
        loose_utf8(heap, value, ctx)
    }

    /// `ToString`, as UTF-8 for the call.
    pub fn loose_utf8<'a>(
        heap: &JsHeap<'a>,
        value: &JSValue,
        _: Context<'_>,
    ) -> Result<'a, &'a [u8]> {
        let string = lift(value.to_utf8(heap.global_object()))?;
        Ok(heap.keep(string.into_bytes()))
    }

    /// `b.string.utf8.owned`: as [`strict_utf8`], kept by the body.
    pub fn strict_owned_utf8<'a>(
        heap: &JsHeap<'a>,
        value: &JSValue,
        ctx: Context<'_>,
    ) -> Result<'a, Vec<u8>> {
        if !value.is_string() && !value.is_string_object() {
            return Err(type_must_be(heap, ctx, "a string", value));
        }
        loose_owned_utf8(heap, value, ctx)
    }

    /// `b.string.loose.utf8.owned`: `ToString`, as UTF-8 the body keeps.
    pub fn loose_owned_utf8<'a>(
        heap: &JsHeap<'a>,
        value: &JSValue,
        _: Context<'_>,
    ) -> Result<'a, Vec<u8>> {
        Ok(lift(value.to_utf8(heap.global_object()))?.into_bytes())
    }

    /// The object a dictionary is read from: any object, but an array only with `allow_array`. Anything else is
    /// `<name> must be an object`, the dictionary naming itself.
    pub fn dictionary_object<'a>(
        heap: &JsHeap<'a>,
        value: &JSValue,
        name: &str,
        allow_array: bool,
    ) -> Result<'a, crate::JSObject> {
        match value.get_object() {
            Some(object) if allow_array || !value.is_array() => Ok(object),
            _ => Err(type_must_be(heap, Context::Argument(name), "an object", value)),
        }
    }

    /// A member of a dictionary, as C++'s `DictionaryMemberReader::get`: from the object or a prototype short of
    /// `Object.prototype`, so that a property added to `Object.prototype` is not an option. `undefined` if none has it.
    pub fn dictionary_member<'a>(
        heap: &JsHeap<'a>,
        object: &crate::JSObject,
        name: &str,
    ) -> Result<'a, JSValue> {
        let global = heap.global_object();
        let key = boa_engine::JsString::from(name);
        let object_prototype = global.cx().intrinsics().constructors().object().prototype();
        let mut holder = object.0.clone();
        loop {
            let has = holder.has_own_property(key.clone(), global.cx());
            if lift(global.lift(has))? {
                let value = object.0.get(key, global.cx());
                return Ok(JSValue(lift(global.lift(value))?));
            }
            match holder.prototype() {
                Some(prototype) if prototype != object_prototype => holder = prototype,
                _ => return Ok(JSValue::UNDEFINED),
            }
        }
    }

    /// A member that has no default and is absent: `<name> is required`.
    pub fn required<'a>(heap: &JsHeap<'a>, ctx: Context<'_>) -> Error<'a> {
        let name = ctx.name();
        heap.error_with_code(ErrorCode::INVALID_ARG_TYPE, format_args!("{name} is required"))
    }

    /// `b.string.local`: a string, as UTF-8 for the call. A `String` object is not one: `.allowStringObject` makes it
    /// a string first, with [`string_of_string_object`].
    pub fn local_string<'a>(
        heap: &JsHeap<'a>,
        value: &JSValue,
        ctx: Context<'_>,
    ) -> Result<'a, &'a [u8]> {
        if !value.is_string() {
            return Err(type_must_be(heap, ctx, "a string", value));
        }
        loose_utf8(heap, value, ctx)
    }

    /// C++'s `stringOfStringObject`: `ToString` of a `String` object (which runs its `toString`), anything else as it
    /// is. `.allowStringObject` and `b.any.stringObjectAsString` have it run on each argument, in order, before any is
    /// converted.
    pub fn string_of_string_object<'a>(heap: &JsHeap<'a>, value: &JSValue) -> Result<'a, JSValue> {
        if !value.is_string_object() {
            return Ok(value.clone());
        }
        lift(value.to_js_string(heap.global_object()))
    }

    /// The integer types an argument converts to.
    pub trait Integer: TryFrom<i64> {
        const MIN: i128;
        const MAX: i128;
    }

    macro_rules! integer {
        ($($t:ty),*) => {$(
            impl Integer for $t {
                const MIN: i128 = <$t>::MIN as i128;
                const MAX: i128 = <$t>::MAX as i128;
            }
        )*};
    }
    integer!(u8, u16, i32, u32, i64, u64, usize);
}

/// What `this` is said to be in an error, as C++'s `Bun::determineSpecificType`, Node's `determineSpecificType`:
/// `type number (1)`, `an instance of Foo`, `function bar`, `undefined`. It reads `constructor.name`, which can throw:
/// that exception is the result.
pub fn determine_specific_type(global: &JSGlobalObject, value: &JSValue) -> JsResult<String> {
    use boa_engine::JsValue;
    let engine = value.as_engine();
    if engine.is_null() {
        return Ok("null".to_owned());
    }
    if engine.is_undefined() {
        return Ok("undefined".to_owned());
    }
    if engine.is_number() {
        return Ok(format!("type number ({})", value.to_utf8(global)?));
    }
    if let Some(boolean) = engine.as_boolean() {
        return Ok(format!("type boolean ({boolean})"));
    }
    if let Some(bigint) = engine.as_bigint() {
        return Ok(format!("type bigint ({}n)", bigint.to_string_radix(10)));
    }
    if let Some(symbol) = engine.as_symbol() {
        let description = symbol
            .description()
            .map(|d| d.to_std_string_lossy())
            .unwrap_or_default();
        return Ok(format!("type symbol (Symbol({description}))"));
    }
    if let Some(string) = engine.as_string() {
        // Node's: the first 25 code units of a longer one than 28, in double quotes if it has a single quote.
        let units: Vec<u16> = string.iter().collect();
        let long = units.len() > 28;
        let shown = String::from_utf16_lossy(if long { &units[..25] } else { &units });
        let ellipsis = if long { "..." } else { "" };
        return Ok(if units.contains(&u16::from(b'\'')) {
            format!("type string (\"{}{ellipsis}\")", shown.replace('"', "\\\""))
        } else {
            format!("type string ('{shown}{ellipsis}')")
        });
    }
    let object = engine.as_object().expect("every other value is an object");
    if object.is_callable() {
        // A `name` that is an own data property: reading it runs nothing.
        let name = object
            .borrow()
            .properties()
            .get(&boa_engine::js_string!("name").into())
            .and_then(|descriptor| descriptor.value().and_then(JsValue::as_string))
            .map(|name| name.to_std_string_lossy())
            .unwrap_or_default();
        return Ok(format!("function {name}"));
    }
    let constructor = object.get(boa_engine::js_string!("constructor"), global.cx());
    let constructor = global.lift(constructor)?;
    if constructor.to_boolean() {
        let name = JSValue::from_engine(constructor)
            .get(global, "name")?
            .unwrap_or_default();
        return Ok(format!("an instance of {}", name.to_utf8(global)?));
    }
    // Where C++ prints `util.inspect` of the object, this is its description without script, C++'s
    // `describeObjectWithoutConstructorName` (what an executable prints): the properties are not listed.
    if !object.is::<boa_engine::builtins::object::OrdinaryObject>() {
        return Ok("[Object]".to_owned());
    }
    // An ordinary object has no trap: listing its keys runs nothing.
    let keys = boa_engine::builtins::object::OrdinaryObject::keys(
        &JsValue::undefined(),
        &[object.clone().into()],
        global.cx(),
    );
    let keys = JSValue::from_engine(global.lift(keys)?);
    let has_properties = keys.get_length(global)? > 0;
    Ok(match (object.prototype().is_none(), has_properties) {
        (true, true) => "[Object: null prototype]",
        (true, false) => "[Object: null prototype] {}",
        (false, true) => "[Object]",
        (false, false) => "{}",
    }
    .to_owned())
}

/// What a host function returns for a body's result: its conversion, or the exception it threw.
pub fn host_fn_return<'a, T, E: IntoError<'a>>(
    cx: &mut JsScope<'a>,
    result: core::result::Result<T, E>,
    convert: impl FnOnce(&JsHeap<'a>, T) -> Result<'a, JsLocal<'a>>,
) -> JsResult<JSValue> {
    let error = match result {
        Ok(value) => match convert(&cx.heap, value) {
            Ok(value) => return Ok(value.0),
            Err(error) => error,
        },
        Err(error) => error.into_error(&cx.heap),
    };
    error.into_host_fn_return(cx)
}

/// What a constructor returns for a body's result: the payload of the new object, or the exception it threw.
pub fn host_fn_construct_return<'a, T, E: IntoError<'a>>(
    cx: &mut JsScope<'a>,
    result: core::result::Result<T, E>,
) -> JsResult<T> {
    result.or_else(|error| error.into_error(&cx.heap).into_host_fn_return(cx))
}
