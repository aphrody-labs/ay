//! `Request` (Fetch §5.4), with Bun's messages and defaults (src/runtime/webcore/Request.rs).

use boa_engine::{Context, JsResult, JsString};
use boa_gc::{Finalize, Trace};
use bun_js::class::{Attributes, ClassBuilder};
use bun_js::{CallFrame, ErrorCode, JSGlobalObject, JSValue, JsClass};

use crate::webcore::body::{self, Body};
use crate::webcore::headers::{self, HeaderList, Headers};
use crate::webcore::response::{HasBody, define_body, header_list};
use crate::{cx, lift};

#[derive(Trace, Finalize)]
pub struct Request {
    #[unsafe_ignore_trace]
    pub method: String,
    #[unsafe_ignore_trace]
    pub url: String,
    /// A `Headers`.
    pub headers: JSValue,
    #[unsafe_ignore_trace]
    pub body: Body,
    #[unsafe_ignore_trace]
    pub used: bool,
    /// The `ReadableStream` that `body` answers, made the first time it is asked for.
    pub stream: JSValue,
    /// An `AbortSignal`, made when it is first read if none was given.
    pub signal: JSValue,
    #[unsafe_ignore_trace]
    pub redirect: &'static str,
    #[unsafe_ignore_trace]
    pub keepalive: bool,
    /// The client of a request to `Bun.serve()`, for `server.requestIP()`.
    #[unsafe_ignore_trace]
    pub remote: Option<std::net::SocketAddr>,
}

impl HasBody for Request {
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

/// Fetch's method normalization: the six standard methods in upper case, others as given.
pub fn normalize_method(global: &JSGlobalObject, method: &str) -> bun_js::JsResult<String> {
    let is_token = !method.is_empty()
        && method
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&b));
    if !is_token {
        return Err(global.throw_type_error(format_args!("Invalid HTTP method: '{method}'")));
    }
    let upper = method.to_ascii_uppercase();
    if matches!(upper.as_str(), "CONNECT" | "TRACE" | "TRACK") {
        return Err(global.throw_type_error(format_args!("Forbidden HTTP method: '{method}'")));
    }
    Ok(match upper.as_str() {
        "DELETE" | "GET" | "HEAD" | "OPTIONS" | "POST" | "PUT" | "PATCH" => upper,
        _ => method.to_owned(),
    })
}

fn redirect_mode(global: &JSGlobalObject, value: &JSValue) -> bun_js::JsResult<&'static str> {
    Ok(match value.to_utf8(global)?.as_str() {
        "follow" => "follow",
        "manual" => "manual",
        "error" => "error",
        other => {
            return Err(global.throw_type_error(format_args!(
                "Failed to construct 'Request': The provided value '{other}' is not a valid enum value of type RequestRedirect."
            )));
        }
    })
}

/// The URL of a `RequestInfo` string, absolute.
pub fn parse_url(global: &JSGlobalObject, text: &str) -> bun_js::JsResult<String> {
    match bun_http_rs::Url::parse(text) {
        Ok(url) => Ok(url.to_string()),
        Err(_) => Err(ErrorCode::INVALID_URL.throw(
            global,
            format_args!("Failed to construct 'Request': Invalid URL \"{text}\""),
        )),
    }
}

fn is_url_object(global: &JSGlobalObject, value: &JSValue) -> bun_js::JsResult<bool> {
    let Some(url) = global.to_js_value().get(global, "URL")?.filter(JSValue::is_callable) else {
        return Ok(false);
    };
    let result = value.as_engine().instance_of(url.as_engine(), cx(global));
    headers::lift_bun(global, result)
}

impl Request {
    /// `new Request(input, init)`.
    pub fn from_arguments(global: &JSGlobalObject, input: &JSValue, init: &JSValue) -> bun_js::JsResult<Request> {
        let mut request = Request {
            method: "GET".into(),
            url: String::new(),
            headers: JSValue::UNDEFINED,
            body: Body::Null,
            used: false,
            stream: JSValue::UNDEFINED,
            signal: JSValue::UNDEFINED,
            redirect: "follow",
            keepalive: false,
            remote: None,
        };
        let mut list = HeaderList::default();
        let mut url_text = None;
        if let Some(other) = input.as_class_ref::<Request>() {
            let other = other.borrow(global)?;
            request.method = other.method.clone();
            request.url = other.url.clone();
            request.body = other.body.clone();
            request.signal = other.signal.clone();
            request.redirect = other.redirect;
            request.keepalive = other.keepalive;
            list = header_list(global, &other.headers)?;
        } else if input.is_string() || is_url_object(global, input)? {
            url_text = Some(input.to_utf8(global)?);
        } else if input.get_object().is_some() {
            if let Some(url) = input.get(global, "url")? {
                url_text = Some(url.to_utf8(global)?);
            }
        } else {
            return Err(global.throw(format_args!(
                "Failed to construct 'Request': expected non-empty string or object, got {}",
                input.js_type_string(global)
            )));
        }
        let mut options = vec![];
        if init.get_object().is_some() {
            options.push(init.clone());
        } else if url_text.is_none() && input.as_class_ref::<Request>().is_none() {
            options.push(input.clone());
        }
        let mut implied_type = None;
        for options in options {
            if let Some(method) = options.get(global, "method")?.filter(JSValue::is_truthy) {
                request.method = normalize_method(global, &method.to_utf8(global)?)?;
            }
            if let Some(headers) = options.get(global, "headers")? {
                list = headers::list_from_init(global, &headers)?;
            }
            if let Some(redirect) = options.get(global, "redirect")? {
                request.redirect = redirect_mode(global, &redirect)?;
            }
            if let Some(keepalive) = options.get(global, "keepalive")? {
                request.keepalive = keepalive.to_boolean();
            }
            if let Some(signal) = options.get(global, "signal")? {
                if signal.is_null() {
                    request.signal = JSValue::UNDEFINED;
                } else if is_abort_signal(global, &signal)? {
                    request.signal = signal;
                } else {
                    return Err(global.throw_type_error(format_args!(
                        "Failed to construct 'Request': signal is not of type AbortSignal."
                    )));
                }
            }
            if let Some(body) = options.get(global, "body")? {
                let (extracted, implied) = body::extract(global, &body)?;
                request.body = extracted;
                implied_type = implied;
            }
        }
        if let Some(text) = url_text {
            if text.is_empty() {
                return Err(global.throw(format_args!("Failed to construct 'Request': url is required.")));
            }
            request.url = parse_url(global, &text)?;
        }
        if request.url.is_empty() {
            return Err(global.throw(format_args!("Failed to construct 'Request': url is required.")));
        }
        if let Some(implied) = implied_type
            && !list.has("content-type")
        {
            list.append("content-type", &implied);
        }
        request.headers = headers::create(global, list)?;
        Ok(request)
    }
}

pub fn is_abort_signal(global: &JSGlobalObject, value: &JSValue) -> bun_js::JsResult<bool> {
    let Some(constructor) = global.to_js_value().get(global, "AbortSignal")? else {
        return Ok(false);
    };
    let result = value.as_engine().instance_of(constructor.as_engine(), cx(global));
    headers::lift_bun(global, result)
}

/// `new AbortController().signal`.
pub fn new_signal(global: &JSGlobalObject) -> bun_js::JsResult<JSValue> {
    let context = cx(global);
    let result = (|| -> JsResult<boa_engine::JsValue> {
        let controller = context.global_object().get(JsString::from("AbortController"), context)?;
        let controller = controller
            .as_constructor()
            .ok_or_else(|| body::type_error("AbortController is not a constructor"))?
            .construct(&[], None, context)?;
        controller.get(JsString::from("signal"), context)
    })();
    headers::lift_bun(global, result).map(JSValue::from_engine)
}

const GETTER: Attributes = Attributes {
    writable: false,
    enumerable: true,
    configurable: true,
};

impl JsClass for Request {
    const NAME: &'static str = "Request";
    const LENGTH: usize = 1;

    fn construct(global: &JSGlobalObject, frame: &CallFrame<'_>) -> bun_js::JsResult<Self> {
        Request::from_arguments(global, &frame.argument(0), &frame.argument(1))
    }

    fn define(class: &mut ClassBuilder<'_, Self>) -> bun_js::JsResult<()> {
        define_body(class)?;
        class.getter("method", GETTER, |this, global| Ok(JSValue::from_utf8(this.borrow(global)?.method.as_bytes())))?;
        class.getter("url", GETTER, |this, global| Ok(JSValue::from_utf8(this.borrow(global)?.url.as_bytes())))?;
        class.getter("redirect", GETTER, |this, global| {
            Ok(JSValue::from_utf8(this.borrow(global)?.redirect.as_bytes()))
        })?;
        class.getter("keepalive", GETTER, |this, global| Ok(JSValue::js_boolean(this.borrow(global)?.keepalive)))?;
        class.getter("signal", GETTER, |this, global| {
            let signal = this.borrow(global)?.signal.clone();
            if !signal.is_undefined() {
                return Ok(signal);
            }
            let signal = new_signal(global)?;
            this.borrow_mut(global)?.signal = signal.clone();
            Ok(signal)
        })?;
        class.getter("cache", GETTER, |_, _| Ok(JSValue::from_utf8(b"default")))?;
        class.getter("credentials", GETTER, |_, _| Ok(JSValue::from_utf8(b"include")))?;
        class.getter("mode", GETTER, |_, _| Ok(JSValue::from_utf8(b"navigate")))?;
        class.getter("destination", GETTER, |_, _| Ok(JSValue::from_utf8(b"")))?;
        class.getter("integrity", GETTER, |_, _| Ok(JSValue::from_utf8(b"")))?;
        class.getter("referrerPolicy", GETTER, |_, _| Ok(JSValue::from_utf8(b"")))?;
        class.getter("referrer", GETTER, |this, global| {
            let headers = this.borrow(global)?.headers.clone();
            let referrer = header_list(global, &headers)?.get("referrer").unwrap_or_default();
            Ok(JSValue::from_utf8(referrer.as_bytes()))
        })?;
        class.method(
            "clone",
            0,
            Attributes {
                writable: true,
                enumerable: true,
                configurable: true,
            },
            |this, global, _| {
                let payload = this.borrow(global)?;
                if payload.used {
                    return Err(global.throw_type_error(format_args!("Body already used")));
                }
                let list = header_list(global, &payload.headers)?;
                let clone = Request {
                    method: payload.method.clone(),
                    url: payload.url.clone(),
                    headers: JSValue::UNDEFINED,
                    body: payload.body.clone(),
                    used: false,
                    stream: JSValue::UNDEFINED,
                    signal: payload.signal.clone(),
                    redirect: payload.redirect,
                    keepalive: payload.keepalive,
                    remote: payload.remote,
                };
                drop(payload);
                create(global, clone, list)
            },
        )?;
        Ok(())
    }
}

/// A new `Request` with the fields `list`.
pub fn create(global: &JSGlobalObject, mut request: Request, list: HeaderList) -> bun_js::JsResult<JSValue> {
    request.headers = headers::create(global, list)?;
    bun_js::class::to_js(global, request)
}

pub fn headers_of(global: &JSGlobalObject, request: &Request) -> bun_js::JsResult<HeaderList> {
    Ok(request
        .headers
        .as_class_ref::<Headers>()
        .map(|h| h.borrow(global).map(|h| h.list.clone()))
        .transpose()?
        .unwrap_or_default())
}

pub(crate) fn install(context: &mut Context) -> JsResult<()> {
    let global = JSGlobalObject::from_context(context);
    let constructor = lift(global, bun_js::class::constructor::<Request>(global))?;
    crate::define_global(context, "Request", constructor.into_engine())
}
