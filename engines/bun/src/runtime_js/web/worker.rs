//! `Worker` underneath (src/jsc/web_worker.rs and WebCore's Worker.cpp): each worker is a thread with a realm and an
//! event loop of its own. The realm that makes it holds a handle (its keep-alive, `terminate()`) and one end of the
//! worker's own channel; the worker's realm holds the other end (`postMessage` and `onmessage` of its global scope).
//! What the thread reports (online, an uncaught error, its exit) is a task of the parent's loop.
//!
//! The thread's body is the embedder's ([`Host`]): bun-boa makes the realm, runs the preloads and the entry module,
//! then the loop.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use boa_engine::object::ObjectInitializer;
use boa_engine::object::builtins::JsArray;
use boa_engine::property::Attribute;
use boa_engine::{Context, JsArgs, JsData, JsError, JsNativeError, JsObject, JsResult, JsString, JsValue, js_string};
use boa_gc::{Finalize, Trace};

use super::message_port::{self, Message, Port};
use crate::event_loop::{EventLoop, Remote, RemoteTask};

/// The main thread is 0.
static NEXT_THREAD_ID: AtomicU32 = AtomicU32::new(1);

/// A module a worker runs, as its constructor resolved it.
pub struct Entry {
    /// A path, or the `data:` or `blob:` URL that `source` is the module of.
    pub specifier: String,
    pub source: Option<Vec<u8>>,
}

/// What a worker runs, as its constructor resolved it.
pub struct WorkerOptions {
    pub entry: Entry,
    pub name: String,
    /// `env`: the worker's `process.env`, else a copy of the process's.
    pub env: Option<Vec<(String, String)>>,
    /// `argv`: after `process.argv[0]` and the entry point.
    pub argv: Vec<String>,
    /// `preload`: modules evaluated before the entry point.
    pub preload: Vec<Entry>,
    /// Made by `node:worker_threads`.
    pub node: bool,
}

/// What the thread of a worker starts with.
pub struct WorkerStart {
    pub thread_id: u32,
    pub options: WorkerOptions,
    /// The worker's end of its own channel.
    pub port: Arc<Port>,
    /// `workerData`, with the ports its transfer list moved.
    pub worker_data: Option<Message>,
    pub control: Arc<Control>,
}

/// What `terminate()` reaches: the worker's loop, once it exists.
#[derive(Default)]
pub struct Control {
    state: Mutex<ControlState>,
}

#[derive(Default)]
struct ControlState {
    terminate: bool,
    worker_loop: Option<(Remote, Arc<AtomicBool>)>,
}

impl Control {
    /// The worker's loop exists: a `terminate()` that came before stops it at once.
    pub fn attach(&self, remote: Remote, terminating: Arc<AtomicBool>) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.terminate {
            terminating.store(true, Ordering::Relaxed);
        }
        state.worker_loop = Some((remote, terminating));
    }

    fn terminate(&self) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state.terminate = true;
        if let Some((remote, terminating)) = &state.worker_loop {
            terminating.store(true, Ordering::Relaxed);
            remote.wake();
        }
    }

    /// Whether `terminate()` was called.
    pub fn terminated(&self) -> bool {
        self.state.lock().unwrap_or_else(|e| e.into_inner()).terminate
    }
}

/// What a worker's thread reports to the realm that made it.
pub struct Reporter {
    parent: Remote,
    thread_id: u32,
}

enum Event {
    Online,
    Error { name: String, message: String, stack: String },
    Exit(i32),
}

impl Reporter {
    fn post(&self, event: Event) {
        let thread_id = self.thread_id;
        let task: RemoteTask = Box::new(move |context| dispatch(thread_id, event, context));
        self.parent.post(task);
    }

    /// The entry module is about to run (the `open` event).
    pub fn online(&self) {
        self.post(Event::Online);
    }

    /// What the worker threw that nothing caught.
    pub fn error(&self, name: String, message: String, stack: String) {
        self.post(Event::Error { name, message, stack });
    }
}

/// The embedder's half: where a specifier leads, and the body of a worker's thread (its exit code).
pub struct Host {
    pub resolve: fn(specifier: &str, context: &mut Context) -> JsResult<Entry>,
    pub run: fn(WorkerStart, &Reporter) -> i32,
}

static HOST: OnceLock<Host> = OnceLock::new();

pub fn set_host(host: Host) {
    let _ = HOST.set(host);
}

fn host() -> JsResult<&'static Host> {
    HOST.get().ok_or_else(|| JsNativeError::error().with_message("Worker is not available in this build").into())
}

/// The worker a realm runs in, if it is a worker's.
pub struct Scope {
    pub thread_id: u32,
    pub name: String,
    pub node: bool,
    port: RefCell<Option<Arc<Port>>>,
    worker_data: RefCell<Option<Message>>,
    /// `process.exit(code)` in the worker.
    exit_code: Cell<Option<i32>>,
}

/// Makes `context` the realm of the worker `start` describes: [`ops`] tell its scripts, and its own port and
/// node's `parentPort` wait for the entry module ([`message_port::entry_evaluated`]).
pub fn enter(context: &mut Context, start: &mut WorkerStart) {
    let scope = Scope {
        thread_id: start.thread_id,
        name: start.options.name.clone(),
        node: start.options.node,
        port: RefCell::new(Some(start.port.clone())),
        worker_data: RefCell::new(start.worker_data.take()),
        exit_code: Cell::new(None),
    };
    context.insert_data(scope);
    message_port::defer_until_entry(context);
}

/// The code `process.exit` gave, in a worker.
pub fn exit_code(context: &Context) -> Option<i32> {
    context.get_data::<Scope>().and_then(|scope| scope.exit_code.get())
}

/// `process.exit(code)` in a worker: the worker's loop and script stop, as for `terminate()`, with this code.
/// `None` on the main thread, where the process exits.
pub fn exit(context: &mut Context, code: i32) -> Option<JsError> {
    let scope = context.get_data::<Scope>()?;
    scope.exit_code.set(Some(code));
    let event_loop = EventLoop::of(context);
    event_loop.terminating().store(true, Ordering::Relaxed);
    Some(boa_engine::error::RuntimeLimitError::Terminated.into())
}

/// The handle of a worker in the realm that made it.
#[derive(Trace, Finalize, JsData)]
struct WorkerHandle {
    #[unsafe_ignore_trace]
    thread_id: u32,
    #[unsafe_ignore_trace]
    control: Arc<Control>,
    #[unsafe_ignore_trace]
    refed: Cell<bool>,
    /// Whether it holds a ref on the loop: running and ref'd.
    #[unsafe_ignore_trace]
    holding: Cell<bool>,
    #[unsafe_ignore_trace]
    exited: Cell<bool>,
}

impl WorkerHandle {
    fn update_ref(&self, context: &Context) {
        let hold = self.refed.get() && !self.exited.get();
        if hold != self.holding.get() {
            self.holding.set(hold);
            let reactor = EventLoop::of(context).reactor().clone();
            if hold { reactor.ref_() } else { reactor.unref() }
        }
    }
}

/// The workers this realm made and that have not exited: their objects and handles.
#[derive(Default)]
struct Workers(RefCell<HashMap<u32, (JsObject, JsObject)>>);

fn workers(context: &mut Context) -> &Workers {
    if context.get_data::<Workers>().is_none() {
        context.insert_data(Workers::default());
    }
    context.get_data::<Workers>().expect("inserted above")
}

fn dispatch(thread_id: u32, event: Event, context: &mut Context) -> JsResult<()> {
    let entry = workers(context).0.borrow().get(&thread_id).cloned();
    let Some((object, handle)) = entry else { return Ok(()) };
    let (kind, payload) = match event {
        Event::Online => (0, JsValue::undefined()),
        Event::Error { name, message, stack } => {
            let payload = ObjectInitializer::new(context)
                .property(js_string!("name"), JsString::from(name.as_str()), Attribute::all())
                .property(js_string!("message"), JsString::from(message.as_str()), Attribute::all())
                .property(js_string!("stack"), JsString::from(stack.as_str()), Attribute::all())
                .build();
            (1, payload.into())
        }
        Event::Exit(code) => {
            workers(context).0.borrow_mut().remove(&thread_id);
            if let Some(data) = handle.downcast_ref::<WorkerHandle>() {
                data.exited.set(true);
                data.update_ref(context);
            }
            (2, code.into())
        }
    };
    let hooks = message_port::hooks(context)?;
    hooks
        .worker_event
        .call(&JsValue::undefined(), &[object.into(), kind.into(), payload], context)
        .map(drop)
}

fn strings(value: &JsValue, context: &mut Context) -> JsResult<Vec<String>> {
    let Some(list) = value.as_object() else { return Ok(Vec::new()) };
    let list = JsArray::from_object(list)?;
    let length = list.length(context)?;
    (0..length)
        .map(|i| Ok(list.get(i, context)?.to_string(context)?.to_std_string_escaped()))
        .collect()
}

fn option(options: &JsValue, name: &str, context: &mut Context) -> JsResult<JsValue> {
    match options.as_object() {
        Some(options) => options.get(JsString::from(name), context),
        None => Ok(JsValue::undefined()),
    }
}

/// `spawn(worker, options)`: the worker's handle and the handle of the parent's end of its channel. `options` is
/// `{ specifier, name, env, argv, preload, workerData, transferList, node }`; the specifiers are resolved here, and
/// what does not resolve throws before a thread starts.
fn op_spawn(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let host = host()?;
    let object = args
        .get_or_undefined(0)
        .as_object()
        .ok_or_else(|| JsNativeError::typ().with_message("spawn(worker, options)"))?;
    let options = args.get_or_undefined(1).clone();
    let specifier = option(&options, "specifier", context)?.to_string(context)?.to_std_string_escaped();
    let entry = (host.resolve)(&specifier, context)?;
    let preload = strings(&option(&options, "preload", context)?, context)?
        .iter()
        .map(|p| (host.resolve)(p, context))
        .collect::<JsResult<Vec<_>>>()?;
    let name = match option(&options, "name", context)? {
        name if name.is_undefined() || name.is_null() => String::new(),
        name => name.to_string(context)?.to_std_string_escaped(),
    };
    let env = match option(&options, "env", context)? {
        env if env.is_object() => {
            let env = env.as_object().expect("an object");
            let keys = env.own_property_keys(context)?;
            let mut pairs = Vec::with_capacity(keys.len());
            for key in keys {
                if matches!(key, boa_engine::property::PropertyKey::Symbol(_)) {
                    continue;
                }
                let value = env.get(key.clone(), context)?;
                if value.is_undefined() {
                    continue;
                }
                pairs.push((key.to_string(), value.to_string(context)?.to_std_string_escaped()));
            }
            Some(pairs)
        }
        _ => None,
    };
    let argv = strings(&option(&options, "argv", context)?, context)?;
    let node = option(&options, "node", context)?.to_boolean();
    let worker_data = option(&options, "workerData", context)?;
    let transfer = option(&options, "transferList", context)?;
    let transfer = match transfer.as_object() {
        Some(list) => {
            let list = JsArray::from_object(list)?;
            let length = list.length(context)?;
            (0..length).map(|i| list.get(i, context)).collect::<JsResult<Vec<_>>>()?
        }
        None => Vec::new(),
    };
    let worker_data = message_port::serialize(&worker_data, &transfer, None, context)?;

    let thread_id = NEXT_THREAD_ID.fetch_add(1, Ordering::Relaxed);
    let (parent_end, worker_end) = Port::pair();
    let control = Arc::new(Control::default());
    let start = WorkerStart {
        thread_id,
        options: WorkerOptions { entry, name, env, argv, preload, node },
        port: worker_end,
        worker_data: Some(worker_data),
        control: control.clone(),
    };
    let reporter = Reporter { parent: EventLoop::of(context).remote(), thread_id };
    let run = host.run;
    let spawned = std::thread::Builder::new()
        .name(format!("Bun Worker {thread_id}"))
        .stack_size(16 * 1024 * 1024)
        .spawn(move || {
            let code = run(start, &reporter);
            reporter.post(Event::Exit(code));
        });
    if let Err(error) = spawned {
        return Err(JsNativeError::error().with_message(format!("Worker: {error}")).into());
    }

    let handle = JsObject::from_proto_and_data(
        context.intrinsics().constructors().object().prototype(),
        WorkerHandle {
            thread_id,
            control,
            refed: Cell::new(true),
            holding: Cell::new(false),
            exited: Cell::new(false),
        },
    );
    handle.downcast_ref::<WorkerHandle>().expect("a worker handle").update_ref(context);
    workers(context).0.borrow_mut().insert(thread_id, (object, handle.clone()));
    let port = message_port::bind_handle(parent_end, context);
    Ok(JsArray::from_iter([handle.into(), port.into()], context).into())
}

fn worker_handle<R>(args: &[JsValue], f: impl FnOnce(&WorkerHandle) -> R) -> JsResult<R> {
    let handle = args.get_or_undefined(0).as_object();
    match handle.as_ref().and_then(|h| h.downcast_ref::<WorkerHandle>()) {
        Some(data) => Ok(f(&data)),
        None => Err(JsNativeError::typ().with_message("not a Worker").into()),
    }
}

fn op_terminate(_: &JsValue, args: &[JsValue], _: &mut Context) -> JsResult<JsValue> {
    worker_handle(args, |handle| handle.control.terminate())?;
    Ok(JsValue::undefined())
}

/// `workerRef(handle, refed)`.
fn op_worker_ref(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let refed = args.get_or_undefined(1).to_boolean();
    worker_handle(args, |handle| {
        handle.refed.set(refed);
        handle.update_ref(context);
    })?;
    Ok(JsValue::undefined())
}

fn op_thread_id(_: &JsValue, args: &[JsValue], _: &mut Context) -> JsResult<JsValue> {
    worker_handle(args, |handle| JsValue::from(handle.thread_id))
}

/// `threadInfo()`: `{ isMainThread, threadId, name, node, workerData, port }` of this realm. A worker's `port` (the
/// object of its end of its own channel) and `workerData` are made the first time, here.
fn op_thread_info(_: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let scope = context
        .get_data::<Scope>()
        .map(|scope| (scope.thread_id, scope.name.clone(), scope.node, scope.port.take(), scope.worker_data.take()));
    let Some((thread_id, name, node, port, worker_data)) = scope else {
        let info = ObjectInitializer::new(context)
            .property(js_string!("isMainThread"), true, Attribute::all())
            .property(js_string!("threadId"), 0, Attribute::all())
            .build();
        return Ok(info.into());
    };
    let port = match port {
        Some(port) => {
            let object = message_port::bind(port, context)?;
            message_port::defer_object(&object, context);
            JsValue::from(object)
        }
        None => JsValue::null(),
    };
    let worker_data = match worker_data {
        Some(message) => message_port::deserialize(message, context)?.0,
        None => JsValue::undefined(),
    };
    let info = ObjectInitializer::new(context)
        .property(js_string!("isMainThread"), false, Attribute::all())
        .property(js_string!("threadId"), thread_id, Attribute::all())
        .property(js_string!("name"), JsString::from(name.as_str()), Attribute::all())
        .property(js_string!("node"), node, Attribute::all())
        .property(js_string!("workerData"), worker_data, Attribute::all())
        .property(js_string!("port"), port, Attribute::all())
        .build();
    Ok(info.into())
}

type Op = fn(&JsValue, &[JsValue], &mut Context) -> JsResult<JsValue>;

pub(crate) const OPS: &[(&str, Op, usize)] = &[
    ("spawn", op_spawn, 2),
    ("terminate", op_terminate, 1),
    ("workerRef", op_worker_ref, 2),
    ("workerThreadId", op_thread_id, 1),
    ("threadInfo", op_thread_info, 0),
];
