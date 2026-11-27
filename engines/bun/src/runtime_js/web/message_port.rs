//! `MessagePort`, `MessageChannel` and `BroadcastChannel` underneath (WebCore's MessagePort.cpp,
//! MessagePortChannelRegistry.cpp, BroadcastChannel.cpp): the two ends of a channel, which the realms of any threads
//! hold, and the structured clone of what goes through them, with `ArrayBuffer`s and ports transferred.
//!
//! An end ([`Port`]) is owned by the loop of the realm that holds its object. What the other end posts waits in its
//! queue until it is started, then each message is a task of that loop. Transferring an end detaches it here, and the
//! realm that receives it binds a new object to it, queue and all. The classes are in JavaScript (bun-boa's
//! js/worker.ts): their objects keep a handle under [`Messaging::handle_key`], and [`ops`] are what they call.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, Weak};

use boa_engine::object::ObjectInitializer;
use boa_engine::object::builtins::JsArray;
use boa_engine::property::Attribute;
use boa_engine::value::TryIntoJs;
use boa_engine::{
    Context, JsArgs, JsData, JsError, JsNativeError, JsObject, JsResult, JsString, JsSymbol, JsValue, NativeFunction,
    js_string,
};
use boa_gc::{Finalize, Trace};
use boa_wintertc::store::{HostSerialized, JsValueStore};

use crate::event_loop::{EventLoop, Remote};

static NEXT_PORT: AtomicU64 = AtomicU64::new(1);

/// What one `postMessage` sends: the clone of the message, and the ports its transfer list moved.
pub struct Message {
    data: JsValueStore,
    ports: Vec<Arc<Port>>,
}

/// One end of a channel, or a `BroadcastChannel`.
pub struct Port {
    id: u64,
    /// A `BroadcastChannel`'s name: it has no peer, and what it posts goes to the others of its name.
    broadcast: Option<String>,
    state: Mutex<PortState>,
}

#[derive(Default)]
struct PortState {
    /// What the other end sent, in order.
    queue: VecDeque<Message>,
    peer: Option<Arc<Port>>,
    /// The loop of the realm that holds this end; none while it is in transit.
    owner: Option<Remote>,
    started: bool,
    /// Delivery waits for the worker's entry module (a worker's own port, and node's `parentPort`).
    deferred: bool,
    closed: bool,
    /// The other end closed, or this one: 'close' is still to dispatch, after the queue.
    close_pending: bool,
    /// A task to deliver is posted to the owner.
    scheduled: bool,
}

/// The `BroadcastChannel`s of each name, in the order they were made.
static BROADCAST: Mutex<Option<HashMap<String, Vec<Weak<Port>>>>> = Mutex::new(None);

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|e| e.into_inner())
}

impl Port {
    fn new(broadcast: Option<String>) -> Arc<Port> {
        Arc::new(Port {
            id: NEXT_PORT.fetch_add(1, Ordering::Relaxed),
            broadcast,
            state: Mutex::default(),
        })
    }

    /// Two entangled ends (`MessageChannel`).
    pub fn pair() -> (Arc<Port>, Arc<Port>) {
        let (a, b) = (Port::new(None), Port::new(None));
        lock(&a.state).peer = Some(b.clone());
        lock(&b.state).peer = Some(a.clone());
        (a, b)
    }

    fn state(&self) -> MutexGuard<'_, PortState> {
        lock(&self.state)
    }

    /// Queues `message` for this end; its owner delivers it if it is started.
    fn deliver(self: &Arc<Self>, message: Message) {
        let mut state = self.state();
        if state.closed {
            return;
        }
        state.queue.push_back(message);
        self.schedule(&mut state);
    }

    fn schedule(self: &Arc<Self>, state: &mut PortState) {
        let has_work = (state.started && !state.deferred && !state.queue.is_empty()) || state.close_pending;
        if state.scheduled || !has_work {
            return;
        }
        let Some(owner) = state.owner.clone() else { return };
        let port = self.clone();
        state.scheduled = owner.post(Box::new(move |context| drain(&port, context)));
    }

    /// Disentangles both ends: each dispatches 'close' after what it has queued.
    fn close(self: &Arc<Self>) {
        let peer = {
            let mut state = self.state();
            if state.closed {
                return;
            }
            state.closed = true;
            state.close_pending = true;
            self.schedule(&mut state);
            state.peer.take()
        };
        if let Some(name) = &self.broadcast
            && let Some(registry) = lock(&BROADCAST).as_mut()
            && let Some(channels) = registry.get_mut(name)
        {
            channels.retain(|c| c.upgrade().is_some_and(|c| !Arc::ptr_eq(&c, self)));
        }
        if let Some(peer) = peer {
            let mut state = peer.state();
            state.peer = None;
            state.close_pending = true;
            peer.schedule(&mut state);
        }
    }
}

/// A port's object's handle: the end it holds (none once transferred or closed), and its keep-alive.
#[derive(Trace, Finalize, JsData)]
pub struct PortHandle {
    #[unsafe_ignore_trace]
    port: RefCell<Option<Arc<Port>>>,
    #[unsafe_ignore_trace]
    refed: Cell<bool>,
    /// Whether it holds a ref on the loop: started, ref'd and not closed.
    #[unsafe_ignore_trace]
    holding: Cell<bool>,
}

impl PortHandle {
    pub(crate) fn port(&self) -> Option<Arc<Port>> {
        self.port.borrow().clone()
    }

    fn update_ref(&self, context: &Context) {
        let started = self.port().is_some_and(|port| {
            let state = port.state();
            state.started && !state.closed
        });
        let hold = started && self.refed.get();
        if hold != self.holding.get() {
            self.holding.set(hold);
            let reactor = EventLoop::of(context).reactor().clone();
            if hold { reactor.ref_() } else { reactor.unref() }
        }
    }

    fn release(&self, context: &Context) {
        if self.holding.replace(false) {
            EventLoop::of(context).reactor().unref();
        }
    }
}

/// What the realm's port and worker classes registered, and the port objects bound here, by port id.
pub(crate) struct Messaging {
    pub(crate) handle_key: JsSymbol,
    /// Of a platform object that has no serialization steps (a `MessageChannel`, a `Worker`), or that node's
    /// `markAsUncloneable` marked: cloning it throws.
    uncloneable_key: JsSymbol,
    hooks: RefCell<Option<Hooks>>,
    ports: RefCell<HashMap<u64, (JsObject, JsObject)>>,
    /// The ports whose delivery waits for the entry module, while it has not run.
    deferring: Cell<bool>,
}

#[derive(Clone)]
pub(crate) struct Hooks {
    /// `(port, kind, data, ports)`: 0 a message, 1 a message that did not deserialize, 2 the port closed.
    pub(crate) dispatch: JsObject,
    /// `(handle) => port`: the object of a port that arrives here.
    pub(crate) make_port: JsObject,
    /// `(message, name) => DOMException`.
    pub(crate) dom_exception: JsObject,
    /// `(worker, kind, payload)`: what a worker's thread reports (see `worker.rs`).
    pub(crate) worker_event: JsObject,
}

pub(crate) fn messaging(context: &mut Context) -> &Messaging {
    if context.get_data::<Messaging>().is_none() {
        let handle_key = JsSymbol::new(Some(js_string!("handle"))).expect("a symbol");
        let uncloneable_key = JsSymbol::new(Some(js_string!("uncloneable"))).expect("a symbol");
        context.insert_data(Messaging {
            handle_key,
            uncloneable_key,
            hooks: RefCell::default(),
            ports: RefCell::default(),
            deferring: Cell::new(false),
        });
    }
    context.get_data::<Messaging>().expect("inserted above")
}

pub(crate) fn hooks(context: &mut Context) -> JsResult<Hooks> {
    messaging(context)
        .hooks
        .borrow()
        .clone()
        .ok_or_else(|| JsNativeError::error().with_message("the message port classes are not installed").into())
}

/// A worker's realm: its own port and node's `parentPort` deliver nothing until [`entry_evaluated`].
pub(crate) fn defer_until_entry(context: &mut Context) {
    messaging(context).deferring.set(true);
}

/// The worker's entry module ran: the deferred ports deliver what they have.
pub fn entry_evaluated(context: &mut Context) {
    messaging(context).deferring.set(false);
    let ports: Vec<Arc<Port>> = messaging(context)
        .ports
        .borrow()
        .values()
        .filter_map(|(_, handle)| handle.downcast_ref::<PortHandle>().and_then(|h| h.port()))
        .collect();
    for port in ports {
        let mut state = port.state();
        if state.deferred {
            state.deferred = false;
            port.schedule(&mut state);
        }
    }
}

/// `DOMException(message, name)` of the realm, or an `Error` before the classes are installed.
pub(crate) fn dom_exception(context: &mut Context, message: &str, name: &str) -> JsError {
    let Ok(hooks) = hooks(context) else {
        return JsNativeError::error().with_message(format!("{name}: {message}")).into();
    };
    let args = [JsString::from(message).into(), JsString::from(name).into()];
    match hooks.dom_exception.call(&JsValue::undefined(), &args, context) {
        Ok(error) => JsError::from_opaque(error),
        Err(error) => error,
    }
}

fn data_clone_error(context: &mut Context, message: &str) -> JsError {
    dom_exception(context, message, "DataCloneError")
}

/// The store's own errors read `DataCloneError: …`: as the DOMException they are.
fn as_dom_exception(error: JsError, context: &mut Context) -> JsError {
    let message = error.as_native().map(|e| e.message().to_owned());
    match message.as_deref().and_then(|m| m.strip_prefix("DataCloneError: ")) {
        Some(rest) => data_clone_error(context, rest),
        None => error,
    }
}

fn handle_object(port: Arc<Port>, context: &mut Context) -> JsObject {
    let prototype = context.intrinsics().constructors().object().prototype();
    JsObject::from_proto_and_data(
        prototype,
        PortHandle {
            port: RefCell::new(Some(port)),
            // Node's: a port refs the loop once it listens for messages (`setupPortReferencing`), or `ref()`.
            refed: Cell::new(false),
            holding: Cell::new(false),
        },
    )
}

/// The handle of a port object, or of a handle itself.
fn handle_of(value: &JsValue, context: &mut Context) -> Option<JsObject> {
    let object = value.as_object()?;
    if object.is::<PortHandle>() {
        return Some(object);
    }
    // Without a trap or a getter to run: every object a message holds comes through here.
    if object.is::<boa_engine::builtins::proxy::Proxy>() {
        return None;
    }
    let key = messaging(context).handle_key.clone();
    let descriptor = object.get_own_property(key, context).ok()??;
    let handle = descriptor.value()?.as_object()?;
    handle.is::<PortHandle>().then_some(handle)
}

fn port_handle(value: &JsValue, context: &mut Context) -> JsResult<JsObject> {
    handle_of(value, context).ok_or_else(|| JsNativeError::typ().with_message("not a MessagePort").into())
}

/// Binds `port` to a new object in this realm (one that arrived, or a worker's own).
pub(crate) fn bind(port: Arc<Port>, context: &mut Context) -> JsResult<JsObject> {
    {
        let mut state = port.state();
        state.owner = Some(EventLoop::of(context).remote());
        state.deferred = false;
        port.schedule(&mut state);
    }
    let id = port.id;
    let handle = handle_object(port, context);
    let make_port = hooks(context)?.make_port;
    let object = make_port.call(&JsValue::undefined(), &[handle.clone().into()], context)?;
    let object = object
        .as_object()
        .ok_or_else(|| JsNativeError::typ().with_message("makePort returns an object"))?;
    messaging(context).ports.borrow_mut().insert(id, (object.clone(), handle));
    Ok(object)
}

/// `port` delivers after the worker's entry module, if that has not run yet.
pub(crate) fn defer_object(object: &JsObject, context: &mut Context) {
    if !messaging(context).deferring.get() {
        return;
    }
    if let Some(port) = handle_of(&object.clone().into(), context)
        .and_then(|handle| handle.downcast_ref::<PortHandle>().and_then(|h| h.port()))
    {
        port.state().deferred = true;
    }
}

/// A new end bound to this realm, for `MessageChannel`: its handle.
pub(crate) fn bind_handle(port: Arc<Port>, context: &mut Context) -> JsObject {
    port.state().owner = Some(EventLoop::of(context).remote());
    handle_object(port, context)
}

/// The port leaves this realm: transferred, or closed.
fn unbind(handle: &JsObject, context: &mut Context) -> Option<Arc<Port>> {
    let data = handle.downcast_ref::<PortHandle>()?;
    data.release(context);
    let port = data.port.borrow_mut().take()?;
    messaging(context).ports.borrow_mut().remove(&port.id);
    Some(port)
}

thread_local! {
    /// The ports a `postMessage` transfers, while it serializes: a port in the message has to be one of them.
    static SENDING: RefCell<Vec<JsObject>> = const { RefCell::new(Vec::new()) };
    /// The objects of the ports a message transferred, while it deserializes.
    static RECEIVING: RefCell<Vec<JsObject>> = const { RefCell::new(Vec::new()) };
}

/// A port in a message: the index of its end in the message's transferred ports.
#[derive(Debug)]
struct PortRef(usize);

impl HostSerialized for PortRef {
    fn deserialize(&self, _context: &mut Context) -> JsResult<JsValue> {
        RECEIVING
            .with(|r| r.borrow().get(self.0).cloned())
            .map(JsValue::from)
            .ok_or_else(|| JsNativeError::error().with_message("DataCloneError: a port that was not transferred").into())
    }
}

/// The serialization steps of a `MessagePort`: only one that the transfer list moves.
pub(crate) fn serialize_port(object: &JsObject, context: &mut Context) -> JsResult<Option<Box<dyn HostSerialized>>> {
    if !object.is::<boa_engine::builtins::proxy::Proxy>() {
        let key = messaging(context).uncloneable_key.clone();
        if object.get_own_property(key, context)?.is_some() {
            return Err(data_clone_error(context, "The object could not be cloned."));
        }
    }
    let Some(handle) = handle_of(&object.clone().into(), context) else { return Ok(None) };
    let index = SENDING.with(|s| s.borrow().iter().position(|h| *h == handle));
    match index {
        Some(index) => Ok(Some(Box::new(PortRef(index)))),
        None => Err(data_clone_error(context, "A MessagePort could not be cloned because it was not transferred")),
    }
}

/// The structured clone of `value`, with what `transfer` lists moved out of this realm.
pub(crate) fn serialize(
    value: &JsValue,
    transfer: &[JsValue],
    source: Option<&Arc<Port>>,
    context: &mut Context,
) -> JsResult<Message> {
    let mut buffers = Vec::new();
    let mut handles: Vec<JsObject> = Vec::new();
    for item in transfer {
        let Some(handle) = handle_of(item, context) else {
            buffers.push(item.clone());
            continue;
        };
        let port = handle.downcast_ref::<PortHandle>().and_then(|h| h.port());
        let Some(port) = port else {
            return Err(data_clone_error(context, "MessagePort in transfer list is already detached"));
        };
        if source.is_some_and(|source| Arc::ptr_eq(source, &port)) {
            return Err(data_clone_error(context, "Transfer list contains source port"));
        }
        if handles.contains(&handle) {
            return Err(data_clone_error(context, "Transfer list contains duplicate MessagePort"));
        }
        handles.push(handle);
    }
    SENDING.with(|s| *s.borrow_mut() = handles.clone());
    // What serializing ran (a getter) may have closed a listed port: then nothing is transferred.
    let usable = |handles: &[JsObject]| {
        handles.iter().all(|handle| {
            handle
                .downcast_ref::<PortHandle>()
                .and_then(|h| h.port())
                .is_some_and(|port| !port.state().closed)
        })
    };
    if !usable(&handles) {
        SENDING.with(|s| s.borrow_mut().clear());
        return Err(data_clone_error(context, "MessagePort in transfer list is already detached"));
    }
    let data = JsValueStore::try_from_js_then(value, context, buffers, |context| {
        if usable(&handles) {
            Ok(())
        } else {
            Err(data_clone_error(context, "MessagePort in transfer list is already detached"))
        }
    });
    SENDING.with(|s| s.borrow_mut().clear());
    let data = data.map_err(|error| as_dom_exception(error, context))?;
    let ports = handles.iter().filter_map(|handle| unbind(handle, context)).collect::<Vec<_>>();
    for port in &ports {
        let mut state = port.state();
        state.owner = None;
        state.started = false;
        state.scheduled = false;
    }
    Ok(Message { data, ports })
}

/// The message in this realm: its data, and the objects of the ports it brought.
pub(crate) fn deserialize(message: Message, context: &mut Context) -> JsResult<(JsValue, Vec<JsObject>)> {
    let mut objects = Vec::with_capacity(message.ports.len());
    for port in message.ports {
        objects.push(bind(port, context)?);
    }
    RECEIVING.with(|r| *r.borrow_mut() = objects.clone());
    let data = message.data.try_into_js(context);
    RECEIVING.with(|r| r.borrow_mut().clear());
    Ok((data?, objects))
}

/// One message (or the 'close') of `port`, in its owner's realm; the next one is a task of its own.
fn drain(port: &Arc<Port>, context: &mut Context) -> JsResult<()> {
    let here = EventLoop::of(context).remote();
    let next = {
        let mut state = port.state();
        state.scheduled = false;
        if !state.owner.as_ref().is_some_and(|owner| owner.same_loop(&here)) {
            // Transferred since: its new owner delivers.
            port.schedule(&mut state);
            return Ok(());
        }
        if state.started && !state.deferred && !state.queue.is_empty() {
            state.queue.pop_front().map(Ok)
        } else if state.close_pending && (state.queue.is_empty() || !state.started) {
            state.close_pending = false;
            Some(Err(()))
        } else {
            None
        }
    };
    let bound = messaging(context).ports.borrow().get(&port.id).cloned();
    let result = match (next, bound) {
        (Some(Ok(message)), Some((object, _))) => {
            let hooks = hooks(context)?;
            match deserialize(message, context) {
                Ok((data, ports)) => {
                    let ports = JsArray::from_iter(ports.into_iter().map(JsValue::from), context);
                    hooks
                        .dispatch
                        .call(&JsValue::undefined(), &[object.into(), 0.into(), data, ports.into()], context)
                        .map(drop)
                }
                Err(_) => hooks
                    .dispatch
                    .call(&JsValue::undefined(), &[object.into(), 1.into()], context)
                    .map(drop),
            }
        }
        (Some(Err(())), Some((object, handle))) => {
            let disentangled = {
                let state = port.state();
                state.closed || (state.peer.is_none() && port.broadcast.is_none())
            };
            if disentangled {
                if let Some(handle) = handle.downcast_ref::<PortHandle>() {
                    handle.release(context);
                }
                messaging(context).ports.borrow_mut().remove(&port.id);
            }
            let hooks = hooks(context)?;
            hooks.dispatch.call(&JsValue::undefined(), &[object.into(), 2.into()], context).map(drop)
        }
        _ => Ok(()),
    };
    let mut state = port.state();
    port.schedule(&mut state);
    result
}

// ───────────────────────────── ops ─────────────────────────────

fn arg_handle(args: &[JsValue], i: usize, context: &mut Context) -> JsResult<JsObject> {
    port_handle(args.get_or_undefined(i), context)
}

fn with_handle<R>(args: &[JsValue], context: &mut Context, f: impl FnOnce(&PortHandle, &mut Context) -> R) -> JsResult<R> {
    let handle = arg_handle(args, 0, context)?;
    let data = handle.downcast_ref::<PortHandle>().expect("checked by port_handle");
    Ok(f(&data, context))
}

/// `channel()`: the handles of a new `MessageChannel`'s two ports.
fn op_channel(_: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let (a, b) = Port::pair();
    let (a, b) = (bind_handle(a, context), bind_handle(b, context));
    Ok(JsArray::from_iter([a.into(), b.into()], context).into())
}

/// `bindPort(handle, port)`: `port` is the object of the new handle.
fn op_bind(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let handle = arg_handle(args, 0, context)?;
    let object = args
        .get_or_undefined(1)
        .as_object()
        .ok_or_else(|| JsNativeError::typ().with_message("bindPort(handle, port)"))?;
    let Some(port) = handle.downcast_ref::<PortHandle>().and_then(|h| h.port()) else {
        return Ok(JsValue::undefined());
    };
    messaging(context).ports.borrow_mut().insert(port.id, (object, handle.clone()));
    Ok(JsValue::undefined())
}

/// `post(handle, message, transfer)`.
fn op_post(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let handle = arg_handle(args, 0, context)?;
    let port = handle.downcast_ref::<PortHandle>().and_then(|h| h.port());
    let transfer = match args.get_or_undefined(2).as_object() {
        Some(list) => {
            let list = JsArray::from_object(list)?;
            let length = list.length(context)?;
            (0..length).map(|i| list.get(i, context)).collect::<JsResult<Vec<_>>>()?
        }
        None => Vec::new(),
    };
    let message = serialize(args.get_or_undefined(1), &transfer, port.as_ref(), context)?;
    let Some(port) = port else { return Ok(JsValue::undefined()) };
    if let Some(name) = &port.broadcast {
        if port.state().closed {
            return Ok(JsValue::undefined());
        }
        let others: Vec<Arc<Port>> = lock(&BROADCAST)
            .as_ref()
            .and_then(|registry| registry.get(name))
            .map(|channels| {
                channels.iter().filter_map(Weak::upgrade).filter(|c| !Arc::ptr_eq(c, &port)).collect()
            })
            .unwrap_or_default();
        for other in others {
            other.deliver(Message { data: message.data.clone(), ports: Vec::new() });
        }
        return Ok(JsValue::undefined());
    }
    let peer = port.state().peer.clone();
    if let Some(peer) = peer {
        peer.deliver(message);
    }
    Ok(JsValue::undefined())
}

/// `start(handle)`: what is queued is delivered, and what comes later.
fn op_start(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    with_handle(args, context, |handle, context| {
        if let Some(port) = handle.port() {
            let mut state = port.state();
            if !state.started {
                state.started = true;
                port.schedule(&mut state);
            }
        }
        handle.update_ref(context);
    })?;
    Ok(JsValue::undefined())
}

/// `defer(handle)`: this port delivers after the worker's entry module (node's `parentPort`).
fn op_defer(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let deferring = messaging(context).deferring.get();
    with_handle(args, context, |handle, _| {
        if deferring && let Some(port) = handle.port() {
            port.state().deferred = true;
        }
    })?;
    Ok(JsValue::undefined())
}

fn op_close(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    with_handle(args, context, |handle, context| {
        if let Some(port) = handle.port() {
            port.close();
        }
        handle.update_ref(context);
    })?;
    Ok(JsValue::undefined())
}

fn op_ref(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let refed = args.get_or_undefined(1).to_boolean();
    with_handle(args, context, |handle, context| {
        handle.refed.set(refed);
        handle.update_ref(context);
    })?;
    Ok(JsValue::undefined())
}

fn op_has_ref(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    with_handle(args, context, |handle, _| JsValue::from(handle.holding.get()))
}

/// Whether the port is entangled and not closed (`isMessagePortActive`).
fn op_is_active(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    with_handle(args, context, |handle, _| {
        JsValue::from(handle.port().is_some_and(|port| {
            let state = port.state();
            !state.closed && (state.peer.is_some() || port.broadcast.is_some())
        }))
    })
}

/// `receive(handle)`: node's `receiveMessageOnPort`, `undefined` or `{ message }`.
fn op_receive(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let handle = arg_handle(args, 0, context)?;
    let Some(port) = handle.downcast_ref::<PortHandle>().and_then(|h| h.port()) else {
        return Ok(JsValue::undefined());
    };
    let message = port.state().queue.pop_front();
    let Some(message) = message else { return Ok(JsValue::undefined()) };
    let (data, _) = deserialize(message, context)?;
    let result = ObjectInitializer::new(context).property(js_string!("message"), data, Attribute::all()).build();
    Ok(result.into())
}

/// `broadcast(name)`: the handle of a new `BroadcastChannel`, started.
fn op_broadcast(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let name = args.get_or_undefined(0).to_string(context)?.to_std_string_escaped();
    let port = Port::new(Some(name.clone()));
    lock(&BROADCAST).get_or_insert_default().entry(name).or_default().push(Arc::downgrade(&port));
    Ok(bind_handle(port, context).into())
}

/// `markUncloneable(object)`: structured clone throws a `DataCloneError` for it.
fn op_mark_uncloneable(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    if let Some(object) = args.get_or_undefined(0).as_object() {
        let key = messaging(context).uncloneable_key.clone();
        let descriptor = boa_engine::property::PropertyDescriptor::builder()
            .value(true)
            .writable(false)
            .enumerable(false)
            .configurable(false);
        object.define_property_or_throw(key, descriptor, context)?;
    }
    Ok(JsValue::undefined())
}

/// `isUncloneable(object)`.
fn op_is_uncloneable(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let Some(object) = args.get_or_undefined(0).as_object() else { return Ok(false.into()) };
    let key = messaging(context).uncloneable_key.clone();
    Ok(object.get_own_property(key, context)?.is_some().into())
}

/// `install(hooks)`: the realm's port and worker classes (see [`Hooks`]).
fn op_install(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let hooks = args.get_or_undefined(0);
    let get = |name: &str, context: &mut Context| -> JsResult<JsObject> {
        hooks
            .as_object()
            .map(|o| o.get(JsString::from(name), context))
            .transpose()?
            .and_then(|f| f.as_object())
            .ok_or_else(|| JsNativeError::typ().with_message(format!("install: {name} is not a function")).into())
    };
    let hooks = Hooks {
        dispatch: get("dispatch", context)?,
        make_port: get("makePort", context)?,
        dom_exception: get("domException", context)?,
        worker_event: get("workerEvent", context)?,
    };
    *messaging(context).hooks.borrow_mut() = Some(hooks);
    Ok(JsValue::undefined())
}

type Op = fn(&JsValue, &[JsValue], &mut Context) -> JsResult<JsValue>;

/// The natives of js/worker.ts's port classes, with [`crate::web::worker`]'s.
pub fn ops(context: &mut Context) -> JsResult<JsObject> {
    let key = messaging(context).handle_key.clone();
    let mut ops = ObjectInitializer::new(context);
    let functions: [(&str, Op, usize); 14] = [
        ("markUncloneable", op_mark_uncloneable, 1),
        ("isUncloneable", op_is_uncloneable, 1),
        ("install", op_install, 1),
        ("channel", op_channel, 0),
        ("bindPort", op_bind, 2),
        ("post", op_post, 3),
        ("start", op_start, 1),
        ("defer", op_defer, 1),
        ("close", op_close, 1),
        ("ref", op_ref, 2),
        ("hasRef", op_has_ref, 1),
        ("isActive", op_is_active, 1),
        ("receive", op_receive, 1),
        ("broadcast", op_broadcast, 1),
    ];
    for (name, f, length) in functions {
        ops.function(NativeFunction::from_fn_ptr(f), JsString::from(name), length);
    }
    for (name, f, length) in crate::web::worker::OPS {
        ops.function(NativeFunction::from_fn_ptr(*f), JsString::from(*name), *length);
    }
    ops.property(js_string!("handleKey"), key, Attribute::all());
    Ok(ops.build())
}
