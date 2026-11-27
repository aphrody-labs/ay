//! `Bun.serve()` on `bun_http_rs`'s server: each request becomes a `Request`, the `routes` that match it or the
//! `fetch` handler answer it with a `Response` or a promise of one, and `error` answers what they throw. Bun's
//! defaults (src/runtime/server): port `$BUN_PORT`/`$PORT`/`$NODE_PORT` or 3000, every interface, `server.url` with
//! `localhost`, 500 "Something went wrong!" for an error nothing handles.

use std::cell::{Cell, RefCell};
use std::net::{IpAddr, Ipv6Addr, SocketAddr, ToSocketAddrs};
use std::rc::Rc;

use boa_engine::object::FunctionObjectBuilder;
use boa_engine::object::builtins::JsPromise;
use boa_engine::property::PropertyDescriptor;
use boa_engine::{Context, JsError, JsObject, JsResult, JsString, JsSymbol, JsValue, NativeFunction};
use boa_gc::{Finalize, Trace};
use bun_http_rs::server::{Bind, IncomingRequest, Responder, ResponseHead, Service};
use bun_js::class::{Attributes, ClassBuilder};
use bun_js::{CallFrame, ErrorCode, JSGlobalObject, JSValue, JsClass};

use crate::event_loop::EventLoop;
use crate::webcore::body::{self, Body};
use crate::webcore::headers::{self, HeaderList};
use crate::webcore::request::{self, Request};
use crate::webcore::response::{Response, header_list};
use crate::{cx, lift};

/// A route's answer.
#[derive(Clone)]
enum Handler {
    /// A `Response` given in `routes`, sent to each request.
    Static { status: u16, status_text: String, headers: HeaderList, body: Rc<[u8]> },
    Function(JsObject),
    /// `{ GET, POST, … }`.
    Methods(Vec<(String, Handler)>),
    /// `false`: the `fetch` handler answers.
    Fetch,
}

#[derive(Clone)]
enum Segment {
    Literal(String),
    Param(String),
    Wildcard,
}

#[derive(Clone)]
struct Route {
    segments: Vec<Segment>,
    handler: Handler,
}

impl Route {
    /// Exact routes first, then those with parameters, then wildcards.
    fn rank(&self) -> u8 {
        if self.segments.iter().any(|s| matches!(s, Segment::Wildcard)) {
            2
        } else if self.segments.iter().any(|s| matches!(s, Segment::Param(_))) {
            1
        } else {
            0
        }
    }

    fn matches(&self, path: &str) -> Option<Vec<(String, String)>> {
        let parts: Vec<&str> = path.trim_start_matches('/').split('/').collect();
        let mut params = Vec::new();
        for (i, segment) in self.segments.iter().enumerate() {
            match segment {
                Segment::Wildcard => return Some(params),
                Segment::Literal(literal) => {
                    if parts.get(i) != Some(&literal.as_str()) {
                        return None;
                    }
                }
                Segment::Param(name) => {
                    let part = parts.get(i).filter(|p| !p.is_empty())?;
                    params.push((name.clone(), percent_decode(part)));
                }
            }
        }
        (parts.len() == self.segments.len()).then_some(params)
    }
}

fn percent_decode(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let hex = |b: u8| (b as char).to_digit(16);
        match (bytes[i], bytes.get(i + 1).copied().and_then(hex), bytes.get(i + 2).copied().and_then(hex)) {
            (b'%', Some(h), Some(l)) => {
                out.push((h * 16 + l) as u8);
                i += 3;
            }
            (b, _, _) => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn parse_pattern(path: &str) -> Vec<Segment> {
    path.trim_start_matches('/')
        .split('/')
        .map(|part| match part {
            "*" => Segment::Wildcard,
            _ if part.starts_with(':') => Segment::Param(part[1..].to_owned()),
            _ => Segment::Literal(part.to_owned()),
        })
        .collect()
}

#[derive(Default)]
struct Handlers {
    fetch: Option<JsObject>,
    error: Option<JsObject>,
    routes: Vec<Route>,
}

pub struct ServerState {
    event_loop: Rc<EventLoop>,
    transport: RefCell<Option<Rc<bun_http_rs::server::Server>>>,
    handlers: RefCell<Handlers>,
    /// The `Server` object, while it listens: the second argument of `fetch`.
    object: RefCell<Option<JsObject>>,
    hostname: Option<String>,
    port: Cell<u16>,
    unix: Option<String>,
    tls: bool,
    development: bool,
    id: String,
}

#[derive(Trace, Finalize)]
pub struct Server {
    #[unsafe_ignore_trace]
    state: Rc<ServerState>,
}

struct Adapter(std::rc::Weak<ServerState>);

impl Service for Adapter {
    fn request(&self, responder: Responder, incoming: IncomingRequest) {
        let Some(state) = self.0.upgrade() else { return };
        let event_loop = state.event_loop.clone();
        event_loop.enqueue(move |context| dispatch(&state, responder, incoming, context));
    }
}

/// Where a response goes once it is ready: shared by the callbacks of a promise.
struct Slot {
    state: Rc<ServerState>,
    responder: RefCell<Option<Responder>>,
    /// Whether the `error` handler has been asked already.
    handled_error: Cell<bool>,
}

#[derive(Trace, Finalize)]
struct SlotCapture {
    #[unsafe_ignore_trace]
    slot: Rc<Slot>,
}

impl ServerState {
    fn url(&self) -> String {
        if let Some(unix) = &self.unix {
            return format!("unix://{unix}");
        }
        let protocol = if self.tls { "https" } else { "http" };
        let host = match self.hostname.as_deref() {
            Some(host) if host.parse::<Ipv6Addr>().is_ok() => format!("[{host}]"),
            Some(host) => host.to_owned(),
            None => "localhost".to_owned(),
        };
        let port = self.port.get();
        let default = if self.tls { 443 } else { 80 };
        if port == default {
            format!("{protocol}://{host}/")
        } else {
            format!("{protocol}://{host}:{port}/")
        }
    }

    fn server_value(&self) -> JsValue {
        self.object.borrow().clone().map_or(JsValue::undefined(), Into::into)
    }
}

fn request_value(state: &ServerState, incoming: &IncomingRequest, context: &mut Context) -> JsResult<JsValue> {
    let global = JSGlobalObject::from_context(context);
    let protocol = if state.tls { "https" } else { "http" };
    let url = if incoming.target.starts_with("http://") || incoming.target.starts_with("https://") {
        incoming.target.clone()
    } else {
        let host = match incoming.header(b"host") {
            Some(host) => String::from_utf8_lossy(host).into_owned(),
            None => state.url().trim_start_matches(&format!("{protocol}://")).trim_end_matches('/').to_owned(),
        };
        format!("{protocol}://{host}{}", incoming.target)
    };
    let list = HeaderList::from_pairs(incoming.headers.iter().map(|(n, v)| {
        (crate::webcore::fetch::header_text(n), crate::webcore::fetch::header_text(v))
    }));
    let has_body = !incoming.body.is_empty()
        || incoming.header(b"content-length").is_some_and(|v| v != b"0")
        || incoming.header(b"transfer-encoding").is_some();
    let request = Request {
        method: incoming.method.clone(),
        url,
        headers: JSValue::UNDEFINED,
        body: if has_body { Body::Bytes(Rc::from(incoming.body.as_slice())) } else { Body::Null },
        used: false,
        stream: JSValue::UNDEFINED,
        signal: JSValue::UNDEFINED,
        redirect: "follow",
        keepalive: false,
        remote: incoming.remote,
    };
    lift(global, request::create(global, request, list)).map(JSValue::into_engine)
}

fn dispatch(state: &Rc<ServerState>, responder: Responder, incoming: IncomingRequest, context: &mut Context) -> JsResult<()> {
    let request = request_value(state, &incoming, context)?;
    let path = incoming.target.split(['?', '#']).next().unwrap_or("/");
    let slot = Rc::new(Slot {
        state: state.clone(),
        responder: RefCell::new(Some(responder)),
        handled_error: Cell::new(false),
    });
    let route = {
        let handlers = state.handlers.borrow();
        let mut found = None;
        for rank in 0..3 {
            for route in handlers.routes.iter().filter(|r| r.rank() == rank) {
                if let Some(params) = route.matches(path) {
                    found = Some((route.handler.clone(), params));
                    break;
                }
            }
            if found.is_some() {
                break;
            }
        }
        found
    };
    let mut handler = match route {
        Some((handler, params)) => {
            let object = JsObject::with_object_proto(context.intrinsics());
            for (name, value) in params {
                object.create_data_property_or_throw(JsString::from(name), JsString::from(value), context)?;
            }
            if let Some(request) = request.as_object() {
                request.create_data_property_or_throw(JsString::from("params"), object, context)?;
            }
            handler
        }
        None => Handler::Fetch,
    };
    if let Handler::Methods(methods) = &handler {
        handler = methods
            .iter()
            .find(|(m, _)| m.eq_ignore_ascii_case(&incoming.method))
            .or_else(|| {
                (incoming.method == "HEAD")
                    .then(|| methods.iter().find(|(m, _)| m == "GET"))
                    .flatten()
            })
            .map(|(_, h)| h.clone())
            .unwrap_or(Handler::Fetch);
    }
    let result = match handler {
        Handler::Static { status, status_text, headers, body } => {
            write(&slot, status, &status_text, &headers, &body);
            return Ok(());
        }
        Handler::Function(function) => function.call(&JsValue::undefined(), &[request, state.server_value()], context),
        Handler::Fetch | Handler::Methods(_) => {
            let fetch = state.handlers.borrow().fetch.clone();
            match fetch {
                Some(fetch) => fetch.call(&state.server_value(), &[request, state.server_value()], context),
                None => {
                    write(&slot, 404, "", &HeaderList::default(), b"");
                    return Ok(());
                }
            }
        }
    };
    match result {
        Ok(value) => settle(&slot, value, context),
        Err(error) => fail(&slot, error, context),
    }
}

fn write(slot: &Slot, status: u16, status_text: &str, headers: &HeaderList, body: &[u8]) {
    let Some(responder) = slot.responder.borrow_mut().take() else { return };
    let fields: Vec<(Vec<u8>, Vec<u8>)> =
        headers.iter().map(|(n, v)| (n.as_bytes().to_vec(), v.as_bytes().to_vec())).collect();
    responder.respond(ResponseHead { status, status_text, headers: &fields }, body);
}

fn then(promise: &JsPromise, slot: &Rc<Slot>, context: &mut Context) -> JsResult<()> {
    let on_fulfilled = NativeFunction::from_copy_closure_with_captures(
        |_, args, capture: &SlotCapture, context| {
            settle(&capture.slot, args.first().cloned().unwrap_or_default(), context)?;
            Ok(JsValue::undefined())
        },
        SlotCapture { slot: slot.clone() },
    );
    let on_rejected = NativeFunction::from_copy_closure_with_captures(
        |_, args, capture: &SlotCapture, context| {
            fail(&capture.slot, JsError::from_opaque(args.first().cloned().unwrap_or_default()), context)?;
            Ok(JsValue::undefined())
        },
        SlotCapture { slot: slot.clone() },
    );
    let on_fulfilled = FunctionObjectBuilder::new(context.realm(), on_fulfilled).build();
    let on_rejected = FunctionObjectBuilder::new(context.realm(), on_rejected).build();
    promise.then(Some(on_fulfilled), Some(on_rejected), context)?;
    Ok(())
}

/// Answers with what a handler returned: a `Response`, or a promise of one.
fn settle(slot: &Rc<Slot>, value: JsValue, context: &mut Context) -> JsResult<()> {
    if let Some(promise) = value.as_object().and_then(|o| JsPromise::from_object(o.clone()).ok()) {
        return then(&promise, slot, context);
    }
    let global = JSGlobalObject::from_context(context);
    let value = JSValue::from_engine(value);
    let Some(response) = value.as_class_ref::<Response>() else {
        let received = lift(global, bun_js::bindgen::determine_specific_type(global, &value))?;
        let error = boa_engine::JsNativeError::typ()
            .with_message(format!("Expected a Response object, but received {received}"))
            .into();
        return fail(slot, error, context);
    };
    let (status, status_text, headers_value, body) = {
        let payload = lift(global, response.borrow(global))?;
        (payload.status, payload.status_text.clone(), payload.headers.clone(), payload.body.clone())
    };
    let headers = lift(global, header_list(global, &headers_value))?;
    match body {
        Body::Null => write(slot, status, &status_text, &headers, b""),
        Body::Bytes(bytes) => write(slot, status, &status_text, &headers, &bytes),
        body @ (Body::Incoming(_) | Body::Stream(_)) => {
            let body_promise = body::read(&body, body::Read::Bytes, String::new(), context)?;
            let response = Rc::new((status, status_text, headers));
            let promise = body_promise
                .as_object()
                .and_then(|o| JsPromise::from_object(o.clone()).ok())
                .ok_or_else(|| body::type_error("a promise"))?;
            let deliver = NativeFunction::from_copy_closure_with_captures(
                |_, args, capture: &BodyCapture, context| {
                    let bytes = body::buffer_source_bytes(&args.first().cloned().unwrap_or_default(), context)
                        .unwrap_or_default();
                    let (status, status_text, headers) = &*capture.response;
                    write(&capture.slot, *status, status_text, headers, &bytes);
                    Ok(JsValue::undefined())
                },
                BodyCapture { slot: slot.clone(), response },
            );
            let failed = NativeFunction::from_copy_closure_with_captures(
                |_, args, capture: &SlotCapture, context| {
                    fail(&capture.slot, JsError::from_opaque(args.first().cloned().unwrap_or_default()), context)?;
                    Ok(JsValue::undefined())
                },
                SlotCapture { slot: slot.clone() },
            );
            let deliver = FunctionObjectBuilder::new(context.realm(), deliver).build();
            let failed = FunctionObjectBuilder::new(context.realm(), failed).build();
            promise.then(Some(deliver), Some(failed), context)?;
        }
    }
    Ok(())
}

#[derive(Trace, Finalize)]
struct BodyCapture {
    #[unsafe_ignore_trace]
    slot: Rc<Slot>,
    #[unsafe_ignore_trace]
    response: Rc<(u16, String, HeaderList)>,
}

/// A handler threw or rejected: the `error` handler answers, else 500.
fn fail(slot: &Rc<Slot>, error: JsError, context: &mut Context) -> JsResult<()> {
    let value = error.into_opaque(context)?;
    let on_error = slot.state.handlers.borrow().error.clone();
    if let Some(on_error) = on_error.filter(|_| !slot.handled_error.replace(true)) {
        return match on_error.call(&slot.state.server_value(), &[value], context) {
            Ok(result) if !result.is_undefined() => settle(slot, result, context),
            Ok(_) => {
                internal_error(slot);
                Ok(())
            }
            Err(error) => {
                print_error(error.into_opaque(context)?, context);
                internal_error(slot);
                Ok(())
            }
        };
    }
    print_error(value, context);
    internal_error(slot);
    Ok(())
}

fn internal_error(slot: &Slot) {
    let mut headers = HeaderList::default();
    headers.append("content-type", "text/plain");
    write(slot, 500, "", &headers, b"Something went wrong!");
}

fn print_error(value: JsValue, context: &mut Context) {
    let console = context.global_object().get(JsString::from("console"), context).ok();
    let error = console
        .as_ref()
        .and_then(JsValue::as_object)
        .and_then(|c| c.get(JsString::from("error"), context).ok());
    if let (Some(console), Some(error)) = (console, error.and_then(|e| e.as_callable())) {
        let _ = error.call(&console, &[value], context);
    }
}

// ───────────────────────────── options ─────────────────────────────

fn static_handler(global: &JSGlobalObject, value: &JSValue) -> bun_js::JsResult<Option<Handler>> {
    let Some(response) = value.as_class_ref::<Response>() else { return Ok(None) };
    let payload = response.borrow(global)?;
    let bytes = match payload.body.bytes() {
        Some(bytes) => bytes,
        None => return Err(global.throw_type_error(format_args!("A static route's Response must have its whole body"))),
    };
    Ok(Some(Handler::Static {
        status: payload.status,
        status_text: payload.status_text.clone(),
        headers: header_list(global, &payload.headers)?,
        body: bytes,
    }))
}

fn handler_of(global: &JSGlobalObject, path: &str, value: &JSValue) -> bun_js::JsResult<Handler> {
    if let Some(handler) = static_handler(global, value)? {
        return Ok(handler);
    }
    if value.is_callable() {
        return Ok(Handler::Function(value.as_engine().as_object().expect("callable")));
    }
    if value.as_engine().as_boolean() == Some(false) {
        return Ok(Handler::Fetch);
    }
    if let Some(object) = value.get_object() {
        let mut methods = Vec::new();
        for method in object.keys(global)? {
            let handler = object.get(global, method.as_bytes())?;
            let handler = match static_handler(global, &handler)? {
                Some(handler) => handler,
                None if handler.is_callable() => {
                    Handler::Function(handler.as_engine().as_object().expect("callable"))
                }
                None => {
                    return Err(global.throw_type_error(format_args!(
                        "Route \"{path}\" {method} handler must be a function or a Response"
                    )));
                }
            };
            methods.push((method.to_ascii_uppercase(), handler));
        }
        return Ok(Handler::Methods(methods));
    }
    Err(global.throw_type_error(format_args!(
        "Route \"{path}\" must be a Response, a function or an object of methods"
    )))
}

fn handlers(global: &JSGlobalObject, options: &JSValue, required: bool) -> bun_js::JsResult<Handlers> {
    let mut handlers = Handlers::default();
    if let Some(routes) = options.get(global, "routes")?.or(options.get(global, "static")?) {
        if let Some(object) = routes.get_object() {
            for path in object.keys(global)? {
                let value = object.get(global, path.as_bytes())?;
                handlers.routes.push(Route {
                    segments: parse_pattern(&path),
                    handler: handler_of(global, &path, &value)?,
                });
            }
        }
    }
    if let Some(fetch) = options.get(global, "fetch")?.filter(JSValue::is_truthy) {
        if !fetch.is_callable() {
            return Err(ErrorCode::INVALID_ARG_TYPE.throw(global, format_args!("Expected fetch() to be a function")));
        }
        handlers.fetch = fetch.as_engine().as_object();
    } else if required && handlers.routes.is_empty() {
        return Err(ErrorCode::INVALID_ARG_TYPE.throw(
            global,
            format_args!(
                "Bun.serve() needs either:\n\n  - A routes object:\n     routes: {{\n       \"/path\": {{\n         GET: (req) => new Response(\"Hello\")\n       }}\n     }}\n\n  - Or a fetch handler:\n     fetch: (req) => {{\n       return new Response(\"Hello\")\n     }}\n\nLearn more at https://bun.com/docs/api/http"
            ),
        ));
    }
    if let Some(error) = options.get(global, "error")?.filter(JSValue::is_truthy) {
        if !error.is_callable() {
            return Err(ErrorCode::INVALID_ARG_TYPE.throw(global, format_args!("Expected error to be a function")));
        }
        handlers.error = error.as_engine().as_object();
    }
    Ok(handlers)
}

fn pem(global: &JSGlobalObject, value: &JSValue) -> bun_js::JsResult<Vec<u8>> {
    if value.is_array() {
        let mut out = Vec::new();
        for i in 0..value.get_length(global)? as u32 {
            out.extend(pem(global, &value.get_index(global, i)?)?);
            out.push(b'\n');
        }
        return Ok(out);
    }
    match body::buffer_source_bytes(value.as_engine(), cx(global)) {
        Some(bytes) => Ok(bytes),
        None => Ok(value.to_utf8(global)?.into_bytes()),
    }
}

fn tls_config(global: &JSGlobalObject, options: &JSValue) -> bun_js::JsResult<Option<std::sync::Arc<bun_http_rs::rustls::ServerConfig>>> {
    let Some(tls) = options.get(global, "tls")?.filter(JSValue::is_truthy) else { return Ok(None) };
    let tls = if tls.is_array() { tls.get_index(global, 0)? } else { tls };
    let (Some(cert), Some(key)) = (tls.get(global, "cert")?, tls.get(global, "key")?) else {
        return Ok(None);
    };
    let (cert, key) = (pem(global, &cert)?, pem(global, &key)?);
    bun_http_rs::transport::server_config(&cert, &key)
        .map(Some)
        .map_err(|error| global.throw_type_error(format_args!("Invalid TLS options: {error}")))
}

fn port_of(global: &JSGlobalObject, options: &JSValue) -> bun_js::JsResult<u16> {
    // `port: 0` is an ephemeral port, not the default.
    if let Some(port) = options.get(global, "port")?.filter(|port| !port.is_undefined_or_null()) {
        let number = port.to_number(global)?;
        if !number.is_finite() || number.fract() != 0.0 || !(0.0..=65535.0).contains(&number) {
            return Err(ErrorCode::OUT_OF_RANGE.throw(
                global,
                format_args!(
                    "The value of \"options.port\" is out of range. It must be >= 0 and <= 65535. Received {number}"
                ),
            ));
        }
        return Ok(number as u16);
    }
    for name in ["BUN_PORT", "PORT", "NODE_PORT"] {
        if let Some(port) = std::env::var(name).ok().and_then(|p| p.parse().ok()) {
            return Ok(port);
        }
    }
    Ok(3000)
}

fn bind_addresses(hostname: Option<&str>, port: u16) -> Vec<SocketAddr> {
    match hostname {
        None | Some("") => vec![
            SocketAddr::new(IpAddr::V6(Ipv6Addr::UNSPECIFIED), port),
            SocketAddr::new(IpAddr::from([0, 0, 0, 0]), port),
        ],
        Some(host) => {
            let bare = host.trim_start_matches('[').trim_end_matches(']');
            let mut addresses: Vec<SocketAddr> = (bare, port).to_socket_addrs().map(Iterator::collect).unwrap_or_default();
            addresses.sort_by_key(SocketAddr::is_ipv6);
            addresses
        }
    }
}

fn serve(global: &JSGlobalObject, frame: &CallFrame<'_>) -> bun_js::JsResult<JSValue> {
    let options = frame.argument(0);
    if options.get_object().is_none() {
        return Err(ErrorCode::INVALID_ARG_TYPE.throw(global, format_args!("Bun.serve expects an object")));
    }
    let handlers = handlers(global, &options, true)?;
    let port = port_of(global, &options)?;
    let hostname = match options.get(global, "hostname")?.or(options.get(global, "host")?) {
        Some(host) if !host.is_undefined_or_null() => Some(host.to_utf8(global)?).filter(|h| !h.is_empty()),
        _ => None,
    };
    let unix = options
        .get(global, "unix")?
        .filter(JSValue::is_string)
        .map(|u| u.to_utf8(global))
        .transpose()?
        .filter(|u| !u.is_empty());
    if unix.is_some() && hostname.is_some() {
        return Err(ErrorCode::INVALID_ARG_TYPE.throw(global, format_args!("Cannot specify both hostname and unix")));
    }
    let tls = tls_config(global, &options)?;
    let development = match options.get(global, "development")? {
        Some(value) => value.to_boolean(),
        None => std::env::var("NODE_ENV").map_or(true, |env| env != "production"),
    };
    let id = match options.get(global, "id")? {
        Some(id) if !id.is_undefined_or_null() => id.to_utf8(global)?,
        _ => String::new(),
    };
    let event_loop = EventLoop::of(cx(global));
    let state = Rc::new(ServerState {
        event_loop: event_loop.clone(),
        transport: RefCell::default(),
        handlers: RefCell::new(handlers),
        object: RefCell::default(),
        hostname: hostname.clone(),
        port: Cell::new(port),
        unix: unix.clone(),
        tls: tls.is_some(),
        development,
        id,
    });
    let service: Rc<dyn Service> = Rc::new(Adapter(Rc::downgrade(&state)));
    let listened = match &unix {
        Some(path) => {
            let _ = std::fs::remove_file(path);
            bun_http_rs::server::Server::listen(event_loop.reactor(), Bind::Unix(path.into()), tls, service)
        }
        None => {
            let mut result = Err(std::io::Error::from(std::io::ErrorKind::AddrNotAvailable));
            for address in bind_addresses(hostname.as_deref(), port) {
                result = bun_http_rs::server::Server::listen(event_loop.reactor(), Bind::Tcp(address), tls.clone(), service.clone());
                if result.as_ref().is_err_and(|e| e.kind() == std::io::ErrorKind::AddrInUse) || result.is_ok() {
                    break;
                }
            }
            result
        }
    };
    let transport = match listened {
        Ok(transport) => transport,
        Err(error) => {
            let in_use = error.kind() == std::io::ErrorKind::AddrInUse;
            let message = if in_use {
                format!("Failed to start server. Is port {port} in use?")
            } else {
                format!("Failed to start server: {error}")
            };
            let error_value = global.create_error_instance(format_args!("{message}"));
            let code = if in_use { "EADDRINUSE" } else { "EADDRNOTAVAIL" };
            error_value.put(global, "code", JSValue::from_utf8(code.as_bytes()));
            error_value.put(global, "syscall", JSValue::from_utf8(b"listen"));
            return Err(global.throw_value(error_value));
        }
    };
    if let Some(address) = transport.local_addr() {
        state.port.set(address.port());
    }
    *state.transport.borrow_mut() = Some(transport);
    let object = bun_js::class::to_js(global, Server { state: state.clone() })?;
    *state.object.borrow_mut() = object.as_engine().as_object();
    Ok(object)
}

fn stop(state: &ServerState, close_active: bool) {
    if let Some(transport) = state.transport.borrow_mut().take() {
        transport.stop(close_active);
    }
    state.object.borrow_mut().take();
}

fn resolved(global: &JSGlobalObject) -> bun_js::JsResult<JSValue> {
    let promise = JsPromise::resolve(JsValue::undefined(), cx(global));
    headers::lift_bun(global, promise).map(|p| JSValue::from_engine(p.into()))
}

const HIDDEN: Attributes = Attributes {
    writable: true,
    enumerable: false,
    configurable: true,
};
const GETTER: Attributes = Attributes {
    writable: false,
    enumerable: true,
    configurable: true,
};

impl JsClass for Server {
    const NAME: &'static str = "Server";

    fn define(class: &mut ClassBuilder<'_, Self>) -> bun_js::JsResult<()> {
        class.getter("port", GETTER, |this, global| {
            let state = this.borrow(global)?.state.clone();
            Ok(if state.unix.is_some() { JSValue::UNDEFINED } else { JSValue::js_number(f64::from(state.port.get())) })
        })?;
        class.getter("hostname", GETTER, |this, global| {
            let state = this.borrow(global)?.state.clone();
            Ok(match (&state.unix, &state.hostname) {
                (Some(_), _) => JSValue::UNDEFINED,
                (None, Some(host)) => JSValue::from_utf8(host.as_bytes()),
                (None, None) => JSValue::from_utf8(b"localhost"),
            })
        })?;
        class.getter("url", GETTER, |this, global| {
            let href = this.borrow(global)?.state.url();
            let url = global.to_js_value().get(global, "URL")?.unwrap_or_default();
            let context = cx(global);
            let result = url
                .as_engine()
                .as_constructor()
                .ok_or_else(|| body::type_error("URL is not a constructor"))
                .and_then(|url| url.construct(&[JsString::from(href).into()], None, context));
            headers::lift_bun(global, result).map(|url| JSValue::from_engine(url.into()))
        })?;
        class.getter("protocol", GETTER, |this, global| {
            Ok(JSValue::from_utf8(if this.borrow(global)?.state.tls { b"https" } else { b"http" }))
        })?;
        class.getter("development", GETTER, |this, global| Ok(JSValue::js_boolean(this.borrow(global)?.state.development)))?;
        class.getter("id", GETTER, |this, global| Ok(JSValue::from_utf8(this.borrow(global)?.state.id.as_bytes())))?;
        class.getter("pendingRequests", GETTER, |this, global| {
            let state = this.borrow(global)?.state.clone();
            let pending = state.transport.borrow().as_ref().map_or(0, |t| t.pending_requests());
            Ok(JSValue::js_number(pending as f64))
        })?;
        class.getter("pendingWebSockets", GETTER, |_, _| Ok(JSValue::js_number(0.0)))?;
        class.method("stop", 1, HIDDEN, |this, global, frame| {
            let state = this.borrow(global)?.state.clone();
            stop(&state, frame.argument(0).to_boolean());
            resolved(global)
        })?;
        class.method("ref", 0, HIDDEN, |this, global, _| {
            let state = this.borrow(global)?.state.clone();
            if let Some(transport) = state.transport.borrow().as_ref() {
                transport.set_ref(true);
            }
            Ok(JSValue::UNDEFINED)
        })?;
        class.method("unref", 0, HIDDEN, |this, global, _| {
            let state = this.borrow(global)?.state.clone();
            if let Some(transport) = state.transport.borrow().as_ref() {
                transport.set_ref(false);
            }
            Ok(JSValue::UNDEFINED)
        })?;
        class.method("reload", 1, HIDDEN, |this, global, frame| {
            let state = this.borrow(global)?.state.clone();
            let new = handlers(global, &frame.argument(0), false)?;
            let mut current = state.handlers.borrow_mut();
            if new.fetch.is_some() {
                current.fetch = new.fetch;
            }
            if new.error.is_some() {
                current.error = new.error;
            }
            if frame.argument(0).get(global, "routes")?.is_some() {
                current.routes = new.routes;
            }
            Ok(this.to_js())
        })?;
        class.method("fetch", 1, HIDDEN, |this, global, frame| {
            let state = this.borrow(global)?.state.clone();
            let request = request::Request::from_arguments(global, &frame.argument(0), &frame.argument(1))?;
            let list = request::headers_of(global, &request)?;
            let request = request::create(global, request, list)?;
            let fetch = state.handlers.borrow().fetch.clone();
            let context = cx(global);
            let result = match fetch {
                Some(fetch) => {
                    fetch.call(&state.server_value(), &[request.into_engine(), state.server_value()], context)
                }
                None => Err(body::type_error("fetch() handler is not set")),
            };
            let promise = match result {
                Ok(value) => JsPromise::resolve(value, context),
                Err(error) => JsPromise::reject(error, context),
            };
            headers::lift_bun(global, promise).map(|p| JSValue::from_engine(p.into()))
        })?;
        class.method("requestIP", 1, HIDDEN, |_, global, frame| {
            let Some(request) = frame.argument(0).as_class_ref::<Request>() else {
                return Ok(JSValue::NULL);
            };
            let Some(remote) = request.borrow(global)?.remote else { return Ok(JSValue::NULL) };
            let (address, family) = match remote.ip() {
                IpAddr::V4(ip) => (ip.to_string(), "IPv4"),
                IpAddr::V6(ip) => match ip.to_ipv4_mapped() {
                    Some(v4) => (v4.to_string(), "IPv4"),
                    None => (ip.to_string(), "IPv6"),
                },
            };
            let object = bun_js::JSObject::create(global);
            object.put(global, "address", JSValue::from_utf8(address.as_bytes()));
            object.put(global, "family", JSValue::from_utf8(family.as_bytes()));
            object.put(global, "port", JSValue::js_number(f64::from(remote.port())));
            Ok(object.to_js())
        })?;
        class.method("timeout", 2, HIDDEN, |_, _, _| Ok(JSValue::UNDEFINED))?;
        class.method("upgrade", 1, HIDDEN, |_, _, _| Ok(JSValue::js_boolean(false)))?;
        class.method("publish", 2, HIDDEN, |_, _, _| Ok(JSValue::js_number(0.0)))?;
        class.method("subscriberCount", 1, HIDDEN, |_, _, _| Ok(JSValue::js_number(0.0)))?;
        Ok(())
    }
}

fn dispose(this: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let global = JSGlobalObject::from_context(context);
    if let Some(server) = JSValue::from_engine(this.clone()).as_class_ref::<Server>() {
        let state = lift(global, server.borrow(global))?.state.clone();
        stop(&state, true);
    }
    Ok(JsPromise::resolve(JsValue::undefined(), context)?.into())
}

/// `Bun.serve`, for `Bun` (js/bun.ts).
pub fn serve_function(context: &mut Context) -> JsResult<JsValue> {
    let global = JSGlobalObject::from_context(context);
    let constructor = lift(global, bun_js::class::constructor::<Server>(global))?;
    let prototype = constructor
        .as_engine()
        .as_object()
        .map(|c| c.get(JsString::from("prototype"), context))
        .transpose()?
        .and_then(|p| p.as_object());
    if let Some(prototype) = prototype {
        for (symbol, name) in [
            (JsSymbol::dispose(), "[Symbol.dispose]"),
            (JsSymbol::async_dispose(), "[Symbol.asyncDispose]"),
        ] {
            let function = FunctionObjectBuilder::new(context.realm(), NativeFunction::from_fn_ptr(dispose))
                .name(JsString::from(name))
                .build();
            prototype.define_property_or_throw(
                symbol,
                PropertyDescriptor::builder()
                    .value(function)
                    .writable(true)
                    .enumerable(false)
                    .configurable(true),
                context,
            )?;
        }
    }
    let global = JSGlobalObject::from_context(context);
    Ok(bun_js::JSFunction::create(global, "serve", serve, 1).into_engine())
}
