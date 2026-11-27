//! The body of a `Request` or `Response` (Fetch §5.1 "Body mixin"): bytes that are all there, or bytes still
//! arriving from the network, or a `ReadableStream` given as the body, which is read when the body is. `body` is
//! that stream, or else a stream of the bytes, which arrive as one chunk once they are all there.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use boa_engine::object::builtins::{JsArrayBuffer, JsPromise, JsUint8Array, AlignedVec};
use boa_engine::{Context, JsError, JsNativeError, JsObject, JsResult, JsString, JsValue};
use bun_js::{JSGlobalObject, JSValue};

use boa_gc::{Finalize, Trace};

use crate::cx;
use crate::web::streams::internals::{Reaction, react, upon};
use crate::web::streams::readable_stream::{self, Consume, ReadableStream};
use crate::web::streams::readable_stream_default_controller::{self as readable_controller, SourceAlgorithms};
use crate::webcore::blob::{Blob, throw_system_error};

/// What a body still arriving has so far, and who waits for the rest.
#[derive(Default)]
pub struct Incoming {
    pub bytes: RefCell<Vec<u8>>,
    pub done: Cell<bool>,
    pub error: RefCell<Option<JsValue>>,
    waiters: RefCell<Vec<Box<dyn FnOnce(&mut Context, Result<&[u8], JsValue>) -> JsResult<()>>>>,
}

impl Incoming {
    /// The body has ended; `error` if it failed. Runs the waiters.
    pub fn finish(&self, context: &mut Context, error: Option<JsValue>) -> JsResult<()> {
        self.done.set(true);
        *self.error.borrow_mut() = error.clone();
        let waiters = std::mem::take(&mut *self.waiters.borrow_mut());
        let bytes = self.bytes.borrow().clone();
        for waiter in waiters {
            match &error {
                Some(error) => waiter(context, Err(error.clone()))?,
                None => waiter(context, Ok(&bytes))?,
            }
        }
        Ok(())
    }
}

#[derive(Clone, Default)]
pub enum Body {
    #[default]
    Null,
    Bytes(Rc<[u8]>),
    Incoming(Rc<Incoming>),
    /// A `ReadableStream` given as the body, read when the body is.
    Stream(JSValue),
}

impl Body {
    pub fn is_null(&self) -> bool {
        matches!(self, Body::Null)
    }

    /// The bytes, if they are all there.
    pub fn bytes(&self) -> Option<Rc<[u8]>> {
        match self {
            Body::Null => Some(Rc::from(&[][..])),
            Body::Bytes(bytes) => Some(bytes.clone()),
            Body::Incoming(incoming) if incoming.done.get() && incoming.error.borrow().is_none() => {
                Some(Rc::from(incoming.bytes.borrow().as_slice()))
            }
            Body::Incoming(_) | Body::Stream(_) => None,
        }
    }
}

/// How a body is read.
#[derive(Clone, Copy)]
pub enum Read {
    Text,
    Json,
    ArrayBuffer,
    Bytes,
    Blob,
}

pub fn type_error(message: &str) -> JsError {
    JsNativeError::typ().with_message(message.to_owned()).into()
}

/// UTF-8 with a byte order mark dropped, as Bun's `text()` decodes.
pub fn decode_text(bytes: &[u8]) -> String {
    let bytes = bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(bytes);
    String::from_utf8_lossy(bytes).into_owned()
}

pub fn array_buffer(bytes: &[u8], context: &mut Context) -> JsResult<JsArrayBuffer> {
    JsArrayBuffer::from_byte_block(AlignedVec::from_slice(0, bytes), context)
}

pub fn uint8_array(bytes: &[u8], context: &mut Context) -> JsResult<JsValue> {
    let buffer = array_buffer(bytes, context)?;
    Ok(JsUint8Array::from_array_buffer(buffer, context)?.into())
}

fn json_parse(text: &str, context: &mut Context) -> JsResult<JsValue> {
    let json = context.intrinsics().objects().json();
    let parse = json.get(JsString::from("parse"), context)?;
    parse
        .as_callable()
        .ok_or_else(|| type_error("JSON.parse"))?
        .call(&json.into(), &[JsString::from(text).into()], context)
}

/// The value of a read of `bytes`; `content_type` is the `Blob`'s type.
pub fn convert(read: Read, bytes: &[u8], content_type: &str, context: &mut Context) -> JsResult<JsValue> {
    match read {
        Read::Text => Ok(JsString::from(decode_text(bytes)).into()),
        Read::Json => json_parse(&decode_text(bytes), context),
        Read::ArrayBuffer => Ok(array_buffer(bytes, context)?.into()),
        Read::Bytes => uint8_array(bytes, context),
        Read::Blob => {
            let global = context.global_object();
            let blob = global.get(JsString::from("Blob"), context)?;
            let Some(blob) = blob.as_constructor() else {
                return Err(JsNativeError::error()
                    .with_message("Blob is not implemented on Boa yet")
                    .into());
            };
            let parts = boa_engine::object::builtins::JsArray::from_iter([uint8_array(bytes, context)?], context);
            let options = JsObject::with_object_proto(context.intrinsics());
            options.set(JsString::from("type"), JsString::from(content_type), false, context)?;
            Ok(blob.construct(&[parts.into(), options.into()], Some(&blob), context)?.into())
        }
    }
}

/// A promise of reading `body` once it is all there.
pub fn read(body: &Body, read: Read, content_type: String, context: &mut Context) -> JsResult<JsValue> {
    if let Body::Stream(stream) = body {
        return read_stream(JSGlobalObject::from_context(context), stream, read, content_type);
    }
    if let Some(bytes) = body.bytes() {
        return Ok(match convert(read, &bytes, &content_type, context) {
            Ok(value) => JsPromise::resolve(value, context)?.into(),
            Err(error) => JsPromise::reject(error, context)?.into(),
        });
    }
    let Body::Incoming(incoming) = body else { unreachable!("only an incoming body is not all there") };
    if let Some(error) = incoming.error.borrow().clone() {
        return Ok(JsPromise::reject(JsError::from_opaque(error), context)?.into());
    }
    let (promise, resolvers) = JsPromise::new_pending(context);
    incoming.waiters.borrow_mut().push(Box::new(move |context, result| {
        let settled = match result {
            Ok(bytes) => convert(read, bytes, &content_type, context),
            Err(error) => Err(JsError::from_opaque(error)),
        };
        match settled {
            Ok(value) => resolvers.resolve.call(&JsValue::undefined(), &[value], context)?,
            Err(error) => {
                let error = error.into_opaque(context)?;
                resolvers.reject.call(&JsValue::undefined(), &[error], context)?
            }
        };
        Ok(())
    }));
    Ok(promise.into())
}

/// A read of a stream body: the stream is read to its end, and the bytes converted.
fn read_stream(global: &JSGlobalObject, stream: &JSValue, read: Read, content_type: String) -> JsResult<JsValue> {
    #[derive(Clone, Trace, Finalize)]
    #[boa_gc(unsafe_no_drop)]
    struct Convert {
        #[unsafe_ignore_trace]
        read: Read,
        content_type: String,
    }
    let bytes = crate::lift(global, readable_stream::consume(global, stream, Consume::Bytes))?;
    let converted: Reaction<Convert> = |global, convert_to, bytes| {
        let bytes = crate::engine::buffer_source_bytes(global, &bytes).unwrap_or_default();
        let value = global.lift(convert(convert_to.read, &bytes, &convert_to.content_type, cx(global)))?;
        Ok(JSValue::from_engine(value))
    };
    let convert_to = Convert { read, content_type };
    Ok(react(global, &bytes, convert_to, Some(converted), None).into_engine())
}

/// The `ReadableStream` of a body: its bytes as one chunk, once they are all there.
pub fn to_stream(global: &JSGlobalObject, body: &Body) -> bun_js::JsResult<JSValue> {
    if let Body::Stream(stream) = body {
        return Ok(stream.clone());
    }
    if let Some(bytes) = body.bytes() {
        let chunks = match bytes.is_empty() {
            true => Vec::new(),
            false => vec![crate::engine::uint8_array(global, &bytes)?],
        };
        return readable_stream::of_chunks(global, &chunks);
    }
    let stream = readable_stream::create(global, SourceAlgorithms::None, 0.0)?;
    let controller = readable_stream::stream_of(&stream).borrow(global)?.controller.clone();
    let promise = global.lift(read(body, Read::Bytes, String::new(), cx(global)))?;
    let arrived: Reaction<JSValue> = |global, controller, bytes| {
        if crate::engine::buffer_source_bytes(global, &bytes).is_some_and(|bytes| !bytes.is_empty()) {
            readable_controller::enqueue(global, controller, bytes)?;
        }
        readable_controller::close(global, controller)?;
        Ok(JSValue::UNDEFINED)
    };
    let failed: Reaction<JSValue> = |global, controller, reason| {
        readable_controller::error(global, controller, reason)?;
        Ok(JSValue::UNDEFINED)
    };
    upon(global, &JSValue::from_engine(promise), controller, Some(arrived), Some(failed));
    Ok(stream)
}

/// A `BodyInit` (Fetch §5.2 "extract a body"): its bytes and the `Content-Type` it implies.
pub fn extract(global: &JSGlobalObject, value: &JSValue) -> bun_js::JsResult<(Body, Option<String>)> {
    if value.is_undefined_or_null() {
        return Ok((Body::Null, None));
    }
    if let Some(text) = value.as_string() {
        return Ok((Body::Bytes(Rc::from(text.as_bytes())), Some("text/plain;charset=utf-8".into())));
    }
    if let Some(bytes) = buffer_source_bytes(value.as_engine(), cx(global)) {
        return Ok((Body::Bytes(Rc::from(bytes)), None));
    }
    if let Some(class) = global_class_instance(global, value, "URLSearchParams")? {
        let text = class.to_utf8(global)?;
        return Ok((
            Body::Bytes(Rc::from(text.as_bytes())),
            Some("application/x-www-form-urlencoded;charset=UTF-8".into()),
        ));
    }
    if let Some(blob) = value.as_class_ref::<Blob>() {
        let blob = blob.borrow(global)?;
        let bytes = blob.read_bytes().map_err(|error| throw_system_error(global, &error))?;
        let content_type = (!blob.content_type.is_empty())
            .then(|| String::from_utf8_lossy(blob.content_type.as_slice()).into_owned());
        return Ok((Body::Bytes(Rc::from(bytes)), content_type));
    }
    if value.as_class_ref::<ReadableStream>().is_some() {
        return Ok((Body::Stream(value.clone()), None));
    }
    if global_class_instance(global, value, "FormData")?.is_some() {
        return Err(global.throw(format_args!("A FormData body is not implemented on Boa yet")));
    }
    let text = value.to_utf8(global)?;
    Ok((Body::Bytes(Rc::from(text.as_bytes())), Some("text/plain;charset=utf-8".into())))
}

/// `value` if it is an instance of `globalThis[name]`, when there is one.
fn global_class_instance(global: &JSGlobalObject, value: &JSValue, name: &str) -> bun_js::JsResult<Option<JSValue>> {
    let constructor = global.to_js_value().get(global, name)?;
    let Some(constructor) = constructor.filter(JSValue::is_callable) else {
        return Ok(None);
    };
    let context = cx(global);
    let result = value.as_engine().instance_of(constructor.as_engine(), context);
    let is = crate::webcore::headers::lift_bun(global, result)?;
    Ok(is.then(|| value.clone()))
}

/// The bytes of an `ArrayBuffer`, a typed array or a `DataView`.
pub fn buffer_source_bytes(value: &JsValue, context: &mut Context) -> Option<Vec<u8>> {
    let object = value.as_object()?;
    if let Ok(buffer) = JsArrayBuffer::from_object(object.clone()) {
        return Some(buffer.data().map(|d| d.to_vec()).unwrap_or_default());
    }
    let (buffer, offset, length) = if let Ok(array) = boa_engine::object::builtins::JsTypedArray::from_object(object.clone()) {
        (
            array.buffer(context).ok()?,
            array.byte_offset(context).ok()?,
            array.byte_length(context).ok()?,
        )
    } else if let Ok(view) = boa_engine::object::builtins::JsDataView::from_object(object.clone()) {
        (
            view.buffer(context).ok()?,
            view.byte_offset(context).ok()? as usize,
            view.byte_length(context).ok()? as usize,
        )
    } else {
        return None;
    };
    let buffer = JsArrayBuffer::from_object(buffer.as_object()?.clone()).ok()?;
    let data = buffer.data()?;
    Some(data.get(offset..offset + length)?.to_vec())
}
