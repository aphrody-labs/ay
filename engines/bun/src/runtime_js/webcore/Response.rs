//! `Response` (Fetch §5.5), with Bun's messages (src/runtime/webcore/Response.rs).

use std::rc::Rc;

use boa_engine::{Context, JsResult};
use boa_gc::{Finalize, Trace};
use bun_js::class::{Attributes, ClassBuilder, ClassRef};
use bun_js::{CallFrame, JSGlobalObject, JSValue, JsClass};

use crate::webcore::body::{self, Body, Read};
use crate::webcore::headers::{self, HeaderList, Headers};
use crate::{cx, lift};

#[derive(Trace, Finalize)]
pub struct Response {
    #[unsafe_ignore_trace]
    pub status: u16,
    #[unsafe_ignore_trace]
    pub status_text: String,
    /// A `Headers`.
    pub headers: JSValue,
    #[unsafe_ignore_trace]
    pub url: String,
    #[unsafe_ignore_trace]
    pub redirected: bool,
    #[unsafe_ignore_trace]
    pub body: Body,
    #[unsafe_ignore_trace]
    pub used: bool,
    /// The `ReadableStream` that `body` answers, made the first time it is asked for.
    pub stream: JSValue,
}

/// `ResponseInit`: status, status text and fields.
pub struct Init {
    pub status: u16,
    pub status_text: String,
    pub headers: HeaderList,
}

impl Default for Init {
    fn default() -> Init {
        Init {
            status: 200,
            status_text: String::new(),
            headers: HeaderList::default(),
        }
    }
}

fn status_of(global: &JSGlobalObject, value: &JSValue) -> bun_js::JsResult<u16> {
    let number = value.to_number(global)?;
    let number = if number.is_finite() { number.trunc() } else { 0.0 };
    if (200.0..600.0).contains(&number) || number == 101.0 {
        return Ok(number as u16);
    }
    let error = global.create_range_error_instance(format_args!(
        "The status provided ({number}) must be 101 or in the range of [200, 599]"
    ));
    Err(global.throw_value(error))
}

pub fn init(global: &JSGlobalObject, value: &JSValue) -> bun_js::JsResult<Init> {
    let mut init = Init::default();
    if value.get_object().is_none() {
        return Ok(init);
    }
    if let Some(response) = value.as_class_ref::<Response>() {
        let response = response.borrow(global)?;
        let list = response
            .headers
            .as_class_ref::<Headers>()
            .map(|h| h.borrow(global).map(|h| h.list.clone()))
            .transpose()?
            .unwrap_or_default();
        return Ok(Init {
            status: response.status,
            status_text: response.status_text.clone(),
            headers: list,
        });
    }
    if let Some(headers) = value.get(global, "headers")? {
        init.headers = headers::list_from_init(global, &headers)?;
    }
    if let Some(status) = value.get(global, "status")? {
        init.status = status_of(global, &status)?;
    }
    if let Some(status_text) = value.get(global, "statusText")?.filter(JSValue::is_truthy) {
        init.status_text = status_text.to_utf8(global)?;
    }
    Ok(init)
}

/// A new `Response`.
pub fn create(
    global: &JSGlobalObject,
    init: Init,
    body: Body,
    url: String,
    redirected: bool,
) -> bun_js::JsResult<JSValue> {
    let headers = headers::create(global, init.headers)?;
    bun_js::class::to_js(
        global,
        Response {
            status: init.status,
            status_text: init.status_text,
            headers,
            url,
            redirected,
            body,
            used: false,
            stream: JSValue::UNDEFINED,
        },
    )
}

pub fn header_list(global: &JSGlobalObject, headers: &JSValue) -> bun_js::JsResult<HeaderList> {
    Ok(headers
        .as_class_ref::<Headers>()
        .map(|h| h.borrow(global).map(|h| h.list.clone()))
        .transpose()?
        .unwrap_or_default())
}

fn content_type(global: &JSGlobalObject, headers: &JSValue) -> bun_js::JsResult<String> {
    Ok(header_list(global, headers)?.get("content-type").unwrap_or_default())
}

/// Takes the body for a read: "Body already used" once it was.
pub fn take_body<T: HasBody>(this: &ClassRef<T>, global: &JSGlobalObject) -> bun_js::JsResult<(Body, String)> {
    let headers = this.borrow(global)?.headers().clone();
    let content_type = content_type(global, &headers)?;
    if body_used(this, global)? {
        return Err(global.throw_type_error(format_args!("Body already used")));
    }
    // A read through the Response leaves the body's stream locked, as a reader that is never released would. A
    // stream given as the body is locked by the read itself.
    let (stream, is_stream_body) = {
        let payload = this.borrow(global)?;
        (payload.stream().clone(), matches!(payload.body(), Body::Stream(_)))
    };
    if !stream.is_undefined() && !is_stream_body {
        crate::web::streams::readable_stream_default_reader::acquire(global, &stream)?;
        crate::web::streams::readable_stream::stream_of(&stream).borrow_mut(global)?.disturbed = true;
    }
    let mut payload = this.borrow_mut(global)?;
    if !payload.body().is_null() {
        payload.set_used();
    }
    Ok((payload.body().clone(), content_type))
}

/// `bodyUsed`, as Bun answers it: read, or its stream read or locked.
fn body_used<T: HasBody>(this: &ClassRef<T>, global: &JSGlobalObject) -> bun_js::JsResult<bool> {
    let (used, stream) = {
        let payload = this.borrow(global)?;
        let stream = match payload.body() {
            Body::Stream(stream) => stream.clone(),
            _ => payload.stream().clone(),
        };
        (payload.used(), stream)
    };
    if used || stream.is_undefined() {
        return Ok(used);
    }
    let stream = crate::web::streams::readable_stream::stream_of(&stream);
    let stream = stream.borrow(global)?;
    Ok(stream.disturbed || !stream.reader.is_undefined())
}

/// `body`: `null` without one, else its `ReadableStream`, the same one each time.
fn body_stream<T: HasBody>(this: &ClassRef<T>, global: &JSGlobalObject) -> bun_js::JsResult<JSValue> {
    let (body, stream) = {
        let payload = this.borrow(global)?;
        (payload.body().clone(), payload.stream().clone())
    };
    if !stream.is_undefined() {
        return Ok(stream);
    }
    if body.is_null() {
        return Ok(JSValue::NULL);
    }
    let stream = body::to_stream(global, &body)?;
    this.borrow_mut(global)?.set_stream(stream.clone());
    Ok(stream)
}

/// What `Request` and `Response` share: the Body mixin.
pub trait HasBody: JsClass {
    fn body(&self) -> &Body;
    fn headers(&self) -> &JSValue;
    fn used(&self) -> bool;
    fn set_used(&mut self);
    /// The `ReadableStream` of `body`, `undefined` until it is asked for.
    fn stream(&self) -> &JSValue;
    fn set_stream(&mut self, stream: JSValue);
}

impl HasBody for Response {
    fn body(&self) -> &Body {
        &self.body
    }
    fn headers(&self) -> &JSValue {
        &self.headers
    }
    fn used(&self) -> bool {
        self.used
    }
    fn set_used(&mut self) {
        self.used = true;
    }
    fn stream(&self) -> &JSValue {
        &self.stream
    }
    fn set_stream(&mut self, stream: JSValue) {
        self.stream = stream;
    }
}

const METHOD: Attributes = Attributes {
    writable: true,
    enumerable: true,
    configurable: true,
};
const GETTER: Attributes = Attributes {
    writable: false,
    enumerable: true,
    configurable: true,
};

fn read_body<T: HasBody>(this: &ClassRef<T>, global: &JSGlobalObject, read: Read) -> bun_js::JsResult<JSValue> {
    let (body, content_type) = match take_body(this, global) {
        Ok(taken) => taken,
        Err(error) => {
            let reason = global.take_exception(error);
            let context = cx(global);
            let promise = boa_engine::object::builtins::JsPromise::reject(
                boa_engine::JsError::from_opaque(reason.into_engine()),
                context,
            );
            return headers::lift_bun(global, promise).map(|p| JSValue::from_engine(p.into()));
        }
    };
    let promise = body::read(&body, read, content_type, cx(global));
    headers::lift_bun(global, promise).map(JSValue::from_engine)
}

/// The Body mixin's members on `class`.
pub fn define_body<T: HasBody>(class: &mut ClassBuilder<'_, T>) -> bun_js::JsResult<()> {
    class.method("text", 0, METHOD, |this, global, _| read_body(this, global, Read::Text))?;
    class.method("json", 0, METHOD, |this, global, _| read_body(this, global, Read::Json))?;
    class.method("arrayBuffer", 0, METHOD, |this, global, _| read_body(this, global, Read::ArrayBuffer))?;
    class.method("bytes", 0, METHOD, |this, global, _| read_body(this, global, Read::Bytes))?;
    class.method("blob", 0, METHOD, |this, global, _| read_body(this, global, Read::Blob))?;
    class.method("formData", 0, METHOD, |_, global, _| {
        Err(global.throw(format_args!("formData() is not implemented on Boa yet")))
    })?;
    class.getter("bodyUsed", GETTER, |this, global| Ok(JSValue::js_boolean(body_used(this, global)?)))?;
    class.getter("body", GETTER, body_stream)?;
    class.getter("headers", GETTER, |this, global| Ok(this.borrow(global)?.headers().clone()))?;
    Ok(())
}

fn json_text(global: &JSGlobalObject, value: &JSValue) -> bun_js::JsResult<String> {
    let engine = value.as_engine();
    if engine.is_undefined() || engine.is_symbol() || engine.is_callable() {
        let error = global.create_type_error_instance(format_args!("Value is not JSON serializable"));
        return Err(global.throw_value(error));
    }
    if engine.is_bigint() {
        let error = global.create_type_error_instance(format_args!("Do not know how to serialize a BigInt"));
        return Err(global.throw_value(error));
    }
    let context = cx(global);
    let result = (|| -> JsResult<boa_engine::JsValue> {
        let json = context.intrinsics().objects().json();
        let stringify = json.get(boa_engine::JsString::from("stringify"), context)?;
        stringify
            .as_callable()
            .ok_or_else(|| body::type_error("JSON.stringify"))?
            .call(&json.into(), &[engine.clone()], context)
    })();
    let text = headers::lift_bun(global, result)?;
    JSValue::from_engine(text).to_utf8(global)
}

impl JsClass for Response {
    const NAME: &'static str = "Response";

    fn construct(global: &JSGlobalObject, frame: &CallFrame<'_>) -> bun_js::JsResult<Self> {
        let mut init = init(global, &frame.argument(1))?;
        let (body, implied_type) = body::extract(global, &frame.argument(0))?;
        if let Some(implied) = implied_type
            && !init.headers.has("content-type")
        {
            init.headers.append("content-type", &implied);
        }
        Ok(Response {
            status: init.status,
            status_text: init.status_text,
            headers: headers::create(global, init.headers)?,
            url: String::new(),
            redirected: false,
            body,
            used: false,
            stream: JSValue::UNDEFINED,
        })
    }

    fn define(class: &mut ClassBuilder<'_, Self>) -> bun_js::JsResult<()> {
        define_body(class)?;
        class.getter("status", GETTER, |this, global| Ok(JSValue::js_number(f64::from(this.borrow(global)?.status))))?;
        class.getter("statusText", GETTER, |this, global| {
            Ok(JSValue::from_utf8(this.borrow(global)?.status_text.as_bytes()))
        })?;
        class.getter("ok", GETTER, |this, global| {
            Ok(JSValue::js_boolean((200..300).contains(&this.borrow(global)?.status)))
        })?;
        class.getter("url", GETTER, |this, global| Ok(JSValue::from_utf8(this.borrow(global)?.url.as_bytes())))?;
        class.getter("redirected", GETTER, |this, global| Ok(JSValue::js_boolean(this.borrow(global)?.redirected)))?;
        class.getter("type", GETTER, |this, global| {
            let kind: &[u8] = if this.borrow(global)?.status < 200 { b"error" } else { b"default" };
            Ok(JSValue::from_utf8(kind))
        })?;
        class.method("clone", 0, METHOD, |this, global, _| {
            let payload = this.borrow(global)?;
            if payload.used {
                return Err(global.throw_type_error(format_args!("Body already used")));
            }
            let list = header_list(global, &payload.headers)?;
            let init = Init {
                status: payload.status,
                status_text: payload.status_text.clone(),
                headers: list,
            };
            let (body, url, redirected) = (payload.body.clone(), payload.url.clone(), payload.redirected);
            drop(payload);
            create(global, init, body, url, redirected)
        })?;
        class.static_method("json", 0, METHOD, |global, frame| {
            let text = json_text(global, &frame.argument(0))?;
            let mut init = init(global, &frame.argument(1))?;
            if !init.headers.has("content-type") {
                init.headers.append("content-type", "application/json;charset=utf-8");
            }
            create(global, init, Body::Bytes(Rc::from(text.as_bytes())), String::new(), false)
        })?;
        class.static_method("redirect", 1, METHOD, |global, frame| {
            let location = frame.argument(0).to_utf8(global)?;
            let status_value = frame.argument(1);
            let status = if status_value.is_undefined() {
                302
            } else if status_value.is_number() {
                status_value.to_number(global)? as i32
            } else {
                let init = init(global, &status_value)?;
                if init.status == 200 { 302 } else { i32::from(init.status) }
            };
            if !matches!(status, 301 | 302 | 303 | 307 | 308) {
                let error = global.create_range_error_instance(format_args!(
                    "Failed to execute 'redirect' on 'Response': Invalid status code"
                ));
                return Err(global.throw_value(error));
            }
            // Fetch's redirect: the serialization of the parsed URL; Bun keeps a URL that is not absolute as given.
            let location = bun_http_rs::Url::parse(&location).map_or(location, |url| url.to_string());
            headers::check_value(global, "location", &location)?;
            let mut headers = HeaderList::default();
            headers.append("location", &location);
            let init = Init {
                status: status as u16,
                status_text: String::new(),
                headers,
            };
            create(global, init, Body::Null, String::new(), false)
        })?;
        class.static_method("error", 0, METHOD, |global, _| {
            let init = Init {
                status: 0,
                status_text: String::new(),
                headers: HeaderList::default(),
            };
            create(global, init, Body::Null, String::new(), false)
        })?;
        Ok(())
    }
}

pub(crate) fn install(context: &mut Context) -> JsResult<()> {
    let global = JSGlobalObject::from_context(context);
    let constructor = lift(global, bun_js::class::constructor::<Response>(global))?;
    crate::define_global(context, "Response", constructor.into_engine())
}
