//! `fetch()` on `bun_http_rs`'s client: the request goes out on the realm's reactor, the response resolves the
//! promise once its head is in, and its body fills while the loop runs. Bun's options: `redirect`, `decompress`,
//! `keepalive`, `unix`, `tls: { rejectUnauthorized, ca, serverName }`, `signal`. `data:` URLs are decoded here.

use std::cell::RefCell;
use std::rc::Rc;

use boa_engine::builtins::promise::ResolvingFunctions;
use boa_engine::object::builtins::JsPromise;
use boa_engine::{Context, JsError, JsNativeError, JsObject, JsResult, JsString, JsValue, NativeFunction};
use boa_gc::{Finalize, Trace};
use bun_http_rs::client::{self, Events, Handle, Head};
use bun_http_rs::{Redirect, TlsOptions, Url};
use bun_js::{CallFrame, JSGlobalObject, JSValue};

use crate::event_loop::EventLoop;
use crate::webcore::body::{self, Body, Incoming};
use crate::webcore::headers::{self, HeaderList};
use crate::webcore::request::{self, Request};
use crate::webcore::response::{self, Init};
use crate::{cx, lift};

/// A header value as script sees it: UTF-8 if it is, Latin-1 otherwise.
pub fn header_text(bytes: &[u8]) -> String {
    match std::str::from_utf8(bytes) {
        Ok(text) => text.to_owned(),
        Err(_) => bytes.iter().map(|&b| char::from(b)).collect(),
    }
}

/// The error `fetch()` rejects with, as Bun's (`code`, `path`, `errno`).
pub fn error_value(error: &client::Error, href: &str, context: &mut Context) -> JsValue {
    let object = JsNativeError::typ().with_message(error.message(href)).into_opaque(context);
    let _ = object.set(JsString::from("code"), JsString::from(error.code()), false, context);
    let _ = object.set(JsString::from("path"), JsString::from(href), false, context);
    let errno = match error {
        client::Error::ConnectionRefused(Some(errno)) => -f64::from(*errno),
        _ => 0.0,
    };
    let _ = object.set(JsString::from("errno"), errno, false, context);
    object.into()
}

struct Pending {
    resolvers: Option<ResolvingFunctions>,
    incoming: Rc<Incoming>,
    href: String,
    signal: Option<JsObject>,
}

/// What the client's events become: tasks on the realm's loop.
struct FetchEvents {
    event_loop: Rc<EventLoop>,
    pending: Rc<RefCell<Pending>>,
}

impl Events for FetchEvents {
    fn head(&self, head: Head) {
        let pending = self.pending.clone();
        self.event_loop.enqueue(move |context| {
            let Some(resolvers) = pending.borrow_mut().resolvers.take() else { return Ok(()) };
            let incoming = pending.borrow().incoming.clone();
            let list = HeaderList::from_pairs(
                head.headers
                    .iter()
                    .map(|(n, v)| (header_text(n), header_text(v))),
            );
            let init = Init {
                status: head.status,
                status_text: header_text(&head.status_text),
                headers: list,
            };
            let global = JSGlobalObject::from_context(context);
            let response = response::create(global, init, Body::Incoming(incoming), head.url.to_string(), head.redirected);
            let response = lift(global, response)?;
            resolvers.resolve.call(&JsValue::undefined(), &[response.into_engine()], context)?;
            Ok(())
        });
    }

    fn body(&self, chunk: &[u8]) {
        self.pending.borrow().incoming.bytes.borrow_mut().extend_from_slice(chunk);
    }

    fn end(&self, result: Result<(), client::Error>) {
        let pending = self.pending.clone();
        self.event_loop.enqueue(move |context| {
            let incoming = pending.borrow().incoming.clone();
            let error = match result {
                Ok(()) => None,
                Err(client::Error::Aborted) => Some(abort_reason(pending.borrow().signal.as_ref(), context)?),
                Err(error) => Some(error_value(&error, &pending.borrow().href, context)),
            };
            let resolvers = pending.borrow_mut().resolvers.take();
            match (resolvers, error) {
                (Some(resolvers), Some(error)) => {
                    resolvers.reject.call(&JsValue::undefined(), &[error], context)?;
                    Ok(())
                }
                (_, error) => incoming.finish(context, error),
            }
        });
    }
}

fn abort_reason(signal: Option<&JsObject>, context: &mut Context) -> JsResult<JsValue> {
    match signal {
        Some(signal) => signal.get(JsString::from("reason"), context),
        None => Ok(JsNativeError::error().with_message("The operation was aborted.").into_opaque(context).into()),
    }
}

#[derive(Trace, Finalize)]
struct OnAbort {
    #[unsafe_ignore_trace]
    handle: Handle,
}

fn tls_options(global: &JSGlobalObject, init: &JSValue) -> bun_js::JsResult<TlsOptions> {
    let mut options = TlsOptions::default();
    let Some(tls) = init.get(global, "tls")?.filter(|t| t.get_object().is_some()) else {
        return Ok(options);
    };
    if let Some(reject) = tls.get(global, "rejectUnauthorized")? {
        options.insecure = !reject.to_boolean();
    }
    if let Some(name) = tls.get(global, "serverName")?.or(tls.get(global, "servername")?) {
        options.server_name = Some(name.to_utf8(global)?);
    }
    if let Some(ca) = tls.get(global, "ca")? {
        let items = if ca.is_array() {
            let length = ca.get_length(global)?;
            (0..length as u32).map(|i| ca.get_index(global, i)).collect::<bun_js::JsResult<Vec<_>>>()?
        } else {
            vec![ca]
        };
        for item in items {
            let bytes = match body::buffer_source_bytes(item.as_engine(), cx(global)) {
                Some(bytes) => bytes,
                None => item.to_utf8(global)?.into_bytes(),
            };
            options.ca.push(bytes);
        }
    }
    Ok(options)
}

/// A `data:` URL's body and type: src/resolver/data_url.rs's `parse_without_check` and `decode_data`, and the
/// `MimeType` the JavaScriptCore build gives the body's blob.
fn data_url(href: &str) -> Option<(Vec<u8>, String)> {
    let rest = href.strip_prefix("data:")?.as_bytes();
    let comma = bun_core::strings::index_of_char_usize(rest, b',')?;
    let (mut mime, data) = (&rest[..comma], &rest[comma + 1..]);
    let base64 = mime.ends_with(b";base64");
    if base64 {
        mime = &mime[..mime.len() - b";base64".len()];
    }
    let mut decoded = Vec::with_capacity(data.len());
    let mut i = 0;
    while i < data.len() {
        let pair = data.get(i + 1..i + 3).and_then(|hex| bun_core::fmt::hex_pair_value(hex[0], hex[1]));
        match (data[i], pair) {
            (b'%', Some(byte)) => {
                decoded.push(byte);
                i += 3;
            }
            (byte, _) => {
                decoded.push(byte);
                i += 1;
            }
        }
    }
    if base64 {
        let bytes = bun_base64::decode_alloc(&decoded).ok()?;
        if bytes.len() != bun_base64::decode_len(&decoded) {
            return None;
        }
        decoded = bytes;
    }
    let mime = bun_http_types::MimeType::MimeType::init(mime);
    Some((decoded, String::from_utf8_lossy(&mime.value).into_owned()))
}

fn rejected(global: &JSGlobalObject, error: bun_js::JsError) -> bun_js::JsResult<JSValue> {
    let reason = global.take_exception(error);
    let promise = JsPromise::reject(JsError::from_opaque(reason.into_engine()), cx(global));
    headers::lift_bun(global, promise).map(|p| JSValue::from_engine(p.into()))
}

fn fetch(global: &JSGlobalObject, frame: &CallFrame<'_>) -> bun_js::JsResult<JSValue> {
    match start(global, frame) {
        Ok(promise) => Ok(promise),
        Err(error) => rejected(global, error),
    }
}

fn start(global: &JSGlobalObject, frame: &CallFrame<'_>) -> bun_js::JsResult<JSValue> {
    let input = frame.argument(0);
    let init = frame.argument(1);
    let request = Request::from_arguments(global, &input, &init)?;
    let list = request::headers_of(global, &request)?;
    let href = request.url.clone();

    let signal = request.signal.get_object().map(|s| s.as_engine().clone());
    if let Some(signal) = &signal {
        let aborted = signal.get(JsString::from("aborted"), cx(global));
        if headers::lift_bun(global, aborted)?.to_boolean() {
            let reason = headers::lift_bun(global, abort_reason(Some(signal), cx(global)))?;
            return Err(global.throw_value(JSValue::from_engine(reason)));
        }
    }

    if href.starts_with("data:") {
        let Some((bytes, mime)) = data_url(&href) else {
            let error = global.create_error_instance(format_args!("failed to fetch the data URL"));
            return Err(global.throw_value(error));
        };
        let mut headers = HeaderList::default();
        headers.append("content-type", &mime);
        let init = Init {
            status: 200,
            status_text: "OK".into(),
            headers,
        };
        let response = response::create(global, init, Body::Bytes(Rc::from(bytes)), href, false)?;
        let promise = JsPromise::resolve(response.into_engine(), cx(global));
        return headers::lift_bun(global, promise).map(|p| JSValue::from_engine(p.into()));
    }

    let url = Url::parse(&href).map_err(|_| {
        bun_js::ErrorCode::INVALID_URL.throw(global, format_args!("fetch() URL is invalid"))
    })?;
    let body = match request.body.bytes() {
        Some(bytes) => bytes.to_vec(),
        None => {
            return Err(global.throw(format_args!(
                "A body that is still being received cannot be sent on Boa yet"
            )));
        }
    };
    let flag = |name: &str, default: bool| -> bun_js::JsResult<bool> {
        Ok(init.get(global, name)?.map_or(default, |v| v.to_boolean()))
    };
    let unix = init
        .get(global, "unix")?
        .filter(JSValue::is_string)
        .map(|path| path.to_utf8(global))
        .transpose()?;
    let client_request = client::Request {
        method: request.method.clone(),
        url,
        headers: list.iter().map(|(n, v)| (n.to_owned(), v.as_bytes().to_vec())).collect(),
        body,
        redirect: match request.redirect {
            "manual" => Redirect::Manual,
            "error" => Redirect::Error,
            _ => Redirect::Follow,
        },
        decompress: flag("decompress", true)?,
        tls: tls_options(global, &init)?,
        unix: unix.map(Into::into),
        user_agent: format!("Bun/{}", bun_core::package_json_version),
        keepalive: flag("keepalive", true)?,
    };

    let context = cx(global);
    let (promise, resolvers) = JsPromise::new_pending(context);
    let event_loop = EventLoop::of(context);
    let pending = Rc::new(RefCell::new(Pending {
        resolvers: Some(resolvers),
        incoming: Rc::new(Incoming::default()),
        href,
        signal: signal.clone(),
    }));
    let events = Rc::new(FetchEvents {
        event_loop: event_loop.clone(),
        pending,
    });
    let handle = client::start(event_loop.reactor(), client_request, events);

    if let Some(signal) = signal {
        let on_abort = NativeFunction::from_copy_closure_with_captures(
            |_, _, captures: &OnAbort, _| {
                captures.handle.abort();
                Ok(JsValue::undefined())
            },
            OnAbort { handle },
        );
        let context = cx(global);
        let listener = boa_engine::object::FunctionObjectBuilder::new(context.realm(), on_abort).build();
        let result = (|| -> JsResult<()> {
            let add = signal.get(JsString::from("addEventListener"), context)?;
            if let Some(add) = add.as_callable() {
                add.call(&signal.clone().into(), &[JsString::from("abort").into(), listener.into()], context)?;
            }
            Ok(())
        })();
        headers::lift_bun(global, result)?;
    }
    Ok(JSValue::from_engine(promise.into()))
}

pub(crate) fn install(context: &mut Context) -> JsResult<()> {
    let global = JSGlobalObject::from_context(context);
    let function = bun_js::JSFunction::create(global, "fetch", fetch, 2);
    let preconnect = bun_js::JSFunction::create(global, "preconnect", |_, _| Ok(JSValue::UNDEFINED), 1);
    if let Some(object) = function.get_object() {
        object.put(global, "preconnect", preconnect);
    }
    crate::define_global(context, "fetch", function.into_engine())
}
