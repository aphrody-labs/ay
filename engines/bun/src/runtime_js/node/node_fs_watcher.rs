//! `fs.watch()`: src/runtime/node/node_fs_watcher.rs on bun_js. `watch(path, options, listener)` of the binding
//! answers an `FSWatcher` with `close()`, `ref()` and `unref()`; the listener is src/js/internal/fs/watch.ts's
//! `#onEvent(eventType, filenameOrError, fromAbort)`. What path_watcher.rs reads of inotify is delivered in a task of
//! the event loop of the realm that watches, each event a call of its own. A persistent watcher keeps that loop
//! alive.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::{Rc, Weak};

use boa_engine::object::FunctionObjectBuilder;
use boa_engine::{Context, JsValue as EngineValue, NativeFunction};
use boa_gc::{Finalize, Trace};
use bun_js::class::{Attributes, ClassBuilder};
use bun_js::{JSGlobalObject, JSValue, JsClass, JsResult};

#[cfg(any(target_os = "linux", target_os = "android"))]
use super::path_watcher::{self, Handle};
use super::types::{ArgumentsSlice, Encoding, invalid_arguments, path_like_from_js, sys_error_to_js};
use crate::{EventLoop, cx, lift};

/// kqueue (FreeBSD), FSEvents (macOS) and libuv (Windows) are not ported: `watch` fails with `ENOTSUP` there.
#[cfg(not(any(target_os = "linux", target_os = "android")))]
mod path_watcher {
    pub(crate) struct Handle;

    pub(crate) fn watch(_: &bun_core::ZStr, _: bool, _: super::HandlerId) -> bun_sys::Result<Handle> {
        Err(bun_sys::Error::from_code(bun_sys::E::ENOTSUP, bun_sys::Tag::watch))
    }

    pub(crate) fn detach(_: Handle, _: super::HandlerId) {}
}
#[cfg(not(any(target_os = "linux", target_os = "android")))]
use path_watcher::Handle;

/// An `FSWatcher` of this thread, which the path watchers know it by.
pub(crate) type HandlerId = u32;

/// What every backend reports of a path.
#[derive(Copy, Clone, Default, Eq, PartialEq, Debug)]
pub(crate) enum WatchEventKind {
    Rename,
    #[default]
    Change,
}

impl WatchEventKind {
    pub(crate) fn to_event(self, path: Box<[u8]>) -> Event {
        Event::Path(self, path)
    }

    fn name(self) -> &'static [u8] {
        match self {
            WatchEventKind::Rename => b"rename",
            WatchEventKind::Change => b"change",
        }
    }
}

pub(crate) enum Event {
    Path(WatchEventKind, Box<[u8]>),
    /// `(event, null)`: the queue overflowed and changes were lost.
    NoFilename(WatchEventKind),
    Error {
        err: bun_sys::Error,
        close: bool,
    },
    Abort,
}

struct Handler {
    listener: JSValue,
    signal: Option<JSValue>,
    /// The `abort` listener on `signal`, removed when the watcher detaches.
    on_abort: Option<JSValue>,
    event_loop: Weak<EventLoop>,
    encoding: Encoding,
    persistent: bool,
    verbose: bool,
    watch: Option<Handle>,
}

thread_local! {
    /// The watchers that are open: a closed one is not here.
    static HANDLERS: RefCell<HashMap<HandlerId, Handler>> = RefCell::default();
    static NEXT_ID: Cell<HandlerId> = const { Cell::new(1) };
}

fn with_handler<R>(id: HandlerId, f: impl FnOnce(&mut Handler) -> R) -> Option<R> {
    HANDLERS.with_borrow_mut(|handlers| handlers.get_mut(&id).map(f))
}

/// `FSWatcher::on_path_update_posix`'s `verbose` lines.
pub(crate) fn log_path_update(id: HandlerId, rel_path: &[u8], is_file: bool) {
    if with_handler(id, |handler| handler.verbose) != Some(true) {
        return;
    }
    let path = String::from_utf8_lossy(rel_path);
    if is_file {
        bun_core::pretty_errorln!("<r> <d>File changed: {}<r>", path);
    } else {
        bun_core::pretty_errorln!("<r> <d>Dir changed: {}<r>", path);
    }
    bun_core::output::flush();
}

/// Queues each event as a task of its watcher's event loop. An event of a watcher that closes first is dropped.
pub(crate) fn deliver(events: Vec<(HandlerId, Event)>) {
    for (id, event) in events {
        let Some(event_loop) = with_handler(id, |handler| handler.event_loop.upgrade()).flatten() else { continue };
        event_loop.enqueue(move |context: &mut Context| {
            let global = JSGlobalObject::from_context(context);
            lift(global, run(global, id, event))
        });
    }
}

fn event_name(name: &[u8]) -> JSValue {
    JSValue::from_utf8(name)
}

fn call_listener(global: &JSGlobalObject, listener: &JSValue, args: &[JSValue]) -> JsResult<()> {
    listener.call(global, global.to_js_value(), args).map(drop)
}

/// The task of one event: a call of the listener, where the watcher is still open.
fn run(global: &JSGlobalObject, id: HandlerId, event: Event) -> JsResult<()> {
    let Some((listener, encoding)) = with_handler(id, |handler| (handler.listener.clone(), handler.encoding)) else {
        return Ok(());
    };
    match event {
        Event::Path(kind, name) => {
            let filename = if name.is_empty() { JSValue::UNDEFINED } else { encoding.to_js(global, &name)? };
            call_listener(global, &listener, &[event_name(kind.name()), filename])
        }
        Event::NoFilename(kind) => call_listener(global, &listener, &[event_name(kind.name()), JSValue::NULL]),
        Event::Error { err, close: closes } => {
            let result = call_listener(global, &listener, &[event_name(b"error"), sys_error_to_js(global, &err)]);
            if closes {
                close(global, id)?;
            }
            result
        }
        Event::Abort => emit_if_aborted(global, id),
    }
}

fn signal_aborted(global: &JSGlobalObject, signal: &JSValue) -> JsResult<bool> {
    Ok(signal.get(global, "aborted")?.is_some_and(|aborted| aborted.to_boolean()))
}

fn emit_if_aborted(global: &JSGlobalObject, id: HandlerId) -> JsResult<()> {
    let Some(Some(signal)) = with_handler(id, |handler| handler.signal.clone()) else {
        return Ok(());
    };
    if !signal_aborted(global, &signal)? {
        return Ok(());
    }
    let reason = signal.get(global, "reason")?.unwrap_or_default();
    emit_abort(global, id, reason)
}

/// `('error', reason, true)`, then the watcher closes whatever the listener did.
fn emit_abort(global: &JSGlobalObject, id: HandlerId, reason: JSValue) -> JsResult<()> {
    let Some(listener) = with_handler(id, |handler| handler.listener.clone()) else {
        return Ok(());
    };
    let reason = if reason.is_undefined_or_null() {
        global.evaluate_script("new DOMException('The operation was aborted.', 'AbortError')")?
    } else {
        reason
    };
    let result = call_listener(global, &listener, &[event_name(b"error"), reason, JSValue::js_boolean(true)]);
    close(global, id)?;
    result
}

/// Stops the watch, then `('close', undefined)`. Once.
fn close(global: &JSGlobalObject, id: HandlerId) -> JsResult<()> {
    let Some(handler) = HANDLERS.with_borrow_mut(|handlers| handlers.remove(&id)) else {
        return Ok(());
    };
    let listener = handler.listener.clone();
    detach(global, id, handler)?;
    call_listener(global, &listener, &[event_name(b"close"), JSValue::UNDEFINED])
}

fn detach(global: &JSGlobalObject, id: HandlerId, handler: Handler) -> JsResult<()> {
    if handler.persistent
        && let Some(event_loop) = handler.event_loop.upgrade()
    {
        event_loop.reactor().unref();
    }
    if let Some(watch) = handler.watch {
        path_watcher::detach(watch, id);
    }
    if let (Some(signal), Some(on_abort)) = (handler.signal, handler.on_abort)
        && let Some(remove) = signal.get(global, "removeEventListener")?
    {
        remove.call(global, signal, &[event_name(b"abort"), on_abort])?;
    }
    Ok(())
}

fn set_persistent(id: HandlerId, persistent: bool) {
    with_handler(id, |handler| {
        if handler.persistent == persistent {
            return;
        }
        handler.persistent = persistent;
        if let Some(event_loop) = handler.event_loop.upgrade() {
            if persistent {
                event_loop.reactor().ref_();
            } else {
                event_loop.reactor().unref();
            }
        }
    });
}

/// The object `watch` answers: the handler's id.
#[derive(Trace, Finalize)]
pub(crate) struct FSWatcher {
    #[unsafe_ignore_trace]
    id: HandlerId,
}

const METHOD: Attributes = Attributes {
    writable: true,
    enumerable: false,
    configurable: true,
};

impl JsClass for FSWatcher {
    const NAME: &'static str = "FSWatcher";

    fn define(class: &mut ClassBuilder<'_, Self>) -> JsResult<()> {
        class.method("close", 0, METHOD, |this, global, _| {
            let id = this.borrow(global)?.id;
            close(global, id)?;
            Ok(JSValue::UNDEFINED)
        })?;
        class.method("ref", 0, METHOD, |this, global, _| {
            set_persistent(this.borrow(global)?.id, true);
            Ok(JSValue::UNDEFINED)
        })?;
        class.method("unref", 0, METHOD, |this, global, _| {
            set_persistent(this.borrow(global)?.id, false);
            Ok(JSValue::UNDEFINED)
        })
    }
}

struct Arguments {
    path: bun_core::ZBox,
    listener: JSValue,
    signal: Option<JSValue>,
    persistent: bool,
    recursive: bool,
    encoding: Encoding,
    verbose: bool,
}

/// `JSValue::get_truthy`: `options[key]`, but for `undefined`, `null` and `""`.
fn get_truthy(global: &JSGlobalObject, options: &JSValue, key: &str) -> JsResult<Option<JSValue>> {
    Ok(options
        .get(global, key)?
        .filter(|value| !value.is_undefined_or_null() && !(value.is_string() && !value.to_boolean())))
}

fn boolean_option(global: &JSGlobalObject, options: &JSValue, key: &str, default: bool) -> JsResult<bool> {
    match get_truthy(global, options, key)? {
        Some(value) if !value.is_boolean() => Err(invalid_arguments(global, format_args!("{key} must be a boolean"))),
        Some(value) => Ok(value.to_boolean()),
        None => Ok(default),
    }
}

fn is_abort_signal(global: &JSGlobalObject, value: &JSValue) -> JsResult<bool> {
    let Some(constructor) = global.to_js_value().get(global, "AbortSignal")? else {
        return Ok(false);
    };
    let context = cx(global);
    let result = value.as_engine().instance_of(constructor.as_engine(), context);
    global.lift(result)
}

impl Arguments {
    /// `Arguments::from_js` of node_fs_watcher.rs.
    fn from_js(global: &JSGlobalObject, args: &[JSValue]) -> JsResult<Arguments> {
        let mut arguments = ArgumentsSlice::new(args, false);
        let Some(path) = path_like_from_js(global, &mut arguments)? else {
            return Err(invalid_arguments(global, format_args!("filename must be a string or TypedArray")));
        };
        let mut listener = None;
        let mut signal = None;
        let mut persistent = true;
        let mut recursive = false;
        let mut encoding = Encoding::Utf8;
        let mut verbose = false;
        let expected_function = || invalid_arguments(global, format_args!("Expected \"listener\" callback to be a function"));
        if let Some(options) = arguments.next_eat() {
            if options.is_object() {
                persistent = boolean_option(global, &options, "persistent", persistent)?;
                verbose = boolean_option(global, &options, "verbose", verbose)?;
                if let Some(value) = options.get(global, "encoding")? {
                    encoding = Encoding::assert(global, &value, encoding)?;
                }
                recursive = boolean_option(global, &options, "recursive", recursive)?;
                if let Some(value) = get_truthy(global, &options, "signal")? {
                    if !is_abort_signal(global, &value)? {
                        return Err(invalid_arguments(global, format_args!("signal is not of type AbortSignal")));
                    }
                    signal = Some(value);
                }
                if let Some(callable) = arguments.next_eat() {
                    if !callable.is_callable() {
                        return Err(expected_function());
                    }
                    listener = Some(callable);
                }
            } else {
                if !options.is_callable() {
                    return Err(expected_function());
                }
                listener = Some(options);
            }
        }
        let Some(listener) = listener else {
            return Err(invalid_arguments(global, format_args!("Expected \"listener\" callback")));
        };
        Ok(Arguments {
            path,
            listener,
            signal,
            persistent,
            recursive,
            encoding,
            verbose,
        })
    }
}

/// The binding's `watch(path, options, listener)`.
pub(crate) fn watch(global: &JSGlobalObject, args: &[JSValue]) -> JsResult<JSValue> {
    let args = Arguments::from_js(global, args)?;
    let watch_error = |errno: u16| {
        let err = bun_sys::Error {
            errno,
            syscall: bun_sys::Tag::watch,
            ..Default::default()
        }
        .with_path(args.path.as_bytes());
        global.throw_value(sys_error_to_js(global, &err))
    };

    let mut slice = args.path.as_bytes();
    if let Some(rest) = slice.strip_prefix(b"file://") {
        slice = rest;
    }
    let cwd = bun_sys::getcwd_alloc().unwrap_or_default();
    let mut file_path = bun_paths::Path::resolve(&[cwd.as_bytes(), slice]);
    // After one, libuv reports `\a` for `a`.
    file_path.copy_trailing_sep_from(slice);
    if file_path.len() >= bun_core::MAX_PATH_BYTES {
        return Err(watch_error(bun_sys::E::ENAMETOOLONG as u16));
    }

    let event_loop: Rc<EventLoop> = EventLoop::of(cx(global));
    let aborted = match &args.signal {
        Some(signal) => signal_aborted(global, signal)?,
        None => false,
    };
    let id = NEXT_ID.get();
    NEXT_ID.set(id.wrapping_add(1).max(1));
    if args.persistent {
        event_loop.reactor().ref_();
    }
    HANDLERS.with_borrow_mut(|handlers| {
        handlers.insert(
            id,
            Handler {
                listener: args.listener.clone(),
                signal: args.signal.clone(),
                on_abort: None,
                event_loop: Rc::downgrade(&event_loop),
                encoding: args.encoding,
                persistent: args.persistent,
                verbose: args.verbose,
                watch: None,
            },
        )
    });

    if !aborted {
        match path_watcher::watch(&file_path, args.recursive, id) {
            Ok(handle) => {
                with_handler(id, |handler| handler.watch = Some(handle));
            }
            Err(err) => {
                if let Some(handler) = HANDLERS.with_borrow_mut(|handlers| handlers.remove(&id)) {
                    detach(global, id, handler)?;
                }
                return Err(watch_error(err.errno));
            }
        }
    }

    if let Some(signal) = &args.signal {
        if aborted {
            // Aborted already: on the next tick.
            deliver(vec![(id, Event::Abort)]);
        } else {
            let on_abort = NativeFunction::from_copy_closure(move |_, _, context| {
                let global = JSGlobalObject::from_context(context);
                lift(global, emit_if_aborted(global, id)).map(|()| EngineValue::undefined())
            });
            let on_abort = JSValue::from_engine(FunctionObjectBuilder::new(cx(global).realm(), on_abort).build().into());
            if let Some(add) = signal.get(global, "addEventListener")? {
                add.call(global, signal.clone(), &[event_name(b"abort"), on_abort.clone()])?;
            }
            with_handler(id, |handler| handler.on_abort = Some(on_abort));
        }
    }

    bun_js::class::to_js(global, FSWatcher { id })
}
