//! `fs.watchFile()`: src/runtime/node/node_fs_stat_watcher.rs on bun_js. The binding's `watchFile(path, options,
//! listener)` answers a `StatWatcher` with `close()`, `ref()` and `unref()`. A timer of the event loop every `interval`
//! ms stats the path again, and calls `listener(current, previous)` when anything but the access time changed. The
//! timer is what keeps the loop alive, while the watcher is persistent.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::{Rc, Weak};
use std::time::Duration;

use boa_engine::NativeFunction;
use boa_engine::object::FunctionObjectBuilder;
use boa_gc::{Finalize, Trace};
use bun_js::class::{Attributes, ClassBuilder};
use bun_js::{ErrorCode, JSGlobalObject, JSValue, JsClass, JsResult};
use bun_sys::PosixStat;

use super::node_fs::stat_path;
use super::stat;
use super::types::{ArgumentsSlice, invalid_arguments, path_like_from_js};
use crate::event_loop::Callback;
use crate::{EventLoop, cx, lift};

struct Watcher {
    path: bun_core::ZBox,
    listener: JSValue,
    /// The stats the listener was last given as `current`.
    prev: JSValue,
    last: PosixStat,
    bigint: bool,
    timer: u32,
    event_loop: Weak<EventLoop>,
}

thread_local! {
    /// The watchers that are open.
    static WATCHERS: RefCell<HashMap<u32, Watcher>> = RefCell::default();
    static NEXT_ID: Cell<u32> = const { Cell::new(1) };
}

/// A stat of the path; a zeroed one where it fails (ENOENT, EPERM), as C++'s.
fn restat(path: &bun_core::ZStr) -> (PosixStat, bool) {
    match stat_path(path, true) {
        Ok(stat) => (stat, true),
        Err(_) => (bun_core::ffi::zeroed(), false),
    }
}

/// The fields `restat` compares: all but the access time.
fn same(a: &PosixStat, b: &PosixStat) -> bool {
    a.dev == b.dev
        && a.ino == b.ino
        && a.mode == b.mode
        && a.nlink == b.nlink
        && a.uid == b.uid
        && a.gid == b.gid
        && a.rdev == b.rdev
        && a.size == b.size
        && a.blksize == b.blksize
        && a.blocks == b.blocks
        && a.mtim.sec == b.mtim.sec
        && a.mtim.nsec == b.mtim.nsec
        && a.ctim.sec == b.ctim.sec
        && a.ctim.nsec == b.ctim.nsec
        && a.birthtim.sec == b.birthtim.sec
        && a.birthtim.nsec == b.birthtim.nsec
}

/// A turn of the watcher's timer.
fn check(global: &JSGlobalObject, id: u32) -> JsResult<()> {
    let Some((path, last, bigint)) =
        WATCHERS.with_borrow(|watchers| watchers.get(&id).map(|w| (w.path.clone(), w.last, w.bigint)))
    else {
        return Ok(());
    };
    let (current, _) = restat(&path);
    if same(&current, &last) {
        return Ok(());
    }
    let current_js = stat::to_js(global, &current, bigint)?;
    let Some((listener, prev)) = WATCHERS.with_borrow_mut(|watchers| {
        let watcher = watchers.get_mut(&id)?;
        watcher.last = current;
        let prev = std::mem::replace(&mut watcher.prev, current_js.clone());
        Some((watcher.listener.clone(), prev))
    }) else {
        return Ok(());
    };
    listener.call(global, JSValue::UNDEFINED, &[current_js, prev]).map(drop)
}

fn close(id: u32) {
    let Some(watcher) = WATCHERS.with_borrow_mut(|watchers| watchers.remove(&id)) else { return };
    if let Some(event_loop) = watcher.event_loop.upgrade() {
        event_loop.clear_timer(watcher.timer);
    }
}

fn set_ref(id: u32, refed: bool) {
    WATCHERS.with_borrow(|watchers| {
        let Some(watcher) = watchers.get(&id) else { return };
        if let Some(event_loop) = watcher.event_loop.upgrade() {
            event_loop.set_timer_ref(watcher.timer, refed);
        }
    });
}

#[derive(Trace, Finalize)]
pub(crate) struct StatWatcher {
    #[unsafe_ignore_trace]
    id: u32,
}

const METHOD: Attributes = Attributes {
    writable: true,
    enumerable: false,
    configurable: true,
};

impl JsClass for StatWatcher {
    const NAME: &'static str = "StatWatcher";

    fn define(class: &mut ClassBuilder<'_, Self>) -> JsResult<()> {
        class.method("close", 0, METHOD, |this, global, _| {
            close(this.borrow(global)?.id);
            Ok(JSValue::UNDEFINED)
        })?;
        class.method("ref", 0, METHOD, |this, global, _| {
            set_ref(this.borrow(global)?.id, true);
            Ok(JSValue::UNDEFINED)
        })?;
        class.method("unref", 0, METHOD, |this, global, _| {
            set_ref(this.borrow(global)?.id, false);
            Ok(JSValue::UNDEFINED)
        })
    }
}

/// `get_boolean_strict`: `undefined` is `None`, anything else but a boolean throws.
fn boolean_strict(global: &JSGlobalObject, options: &JSValue, key: &str) -> JsResult<Option<bool>> {
    let Some(value) = options.get(global, key)? else { return Ok(None) };
    if value.is_boolean() {
        return Ok(Some(value.to_boolean()));
    }
    let actual = value.type_name(global);
    Err(ErrorCode::INVALID_ARG_TYPE.throw(
        global,
        format_args!("The \"{key}\" property must be of type boolean, got {actual}"),
    ))
}

/// The binding's `watchFile(path, options, listener)`, as `Arguments::from_js` and `StatWatcher::init`.
pub(crate) fn watch_file(global: &JSGlobalObject, args: &[JSValue]) -> JsResult<JSValue> {
    let mut arguments = ArgumentsSlice::new(args, false);
    let Some(path) = path_like_from_js(global, &mut arguments)? else {
        return Err(invalid_arguments(global, format_args!("filename must be a string or TypedArray")));
    };
    let mut persistent = true;
    let mut bigint = false;
    let mut interval = 5007;
    if let Some(options) = arguments.next_eat()
        && options.is_object()
    {
        persistent = boolean_strict(global, &options, "persistent")?.unwrap_or(true);
        bigint = boolean_strict(global, &options, "bigint")?.unwrap_or(false);
        if let Some(value) = options.get(global, "interval")? {
            if !value.is_number() {
                return Err(invalid_arguments(global, format_args!("interval must be a number")));
            }
            interval = value.coerce_to_i32(global)?;
        }
    }
    let listener = match arguments.next_eat() {
        Some(listener) if listener.is_callable() => listener,
        _ => return Err(invalid_arguments(global, format_args!("Expected \"listener\" callback"))),
    };

    let mut slice = path.as_bytes();
    if let Some(rest) = slice.strip_prefix(b"file://") {
        slice = rest;
    }
    let cwd = bun_sys::getcwd_alloc().unwrap_or_default();
    let mut file_path = bun_paths::Path::resolve(&[cwd.as_bytes(), slice]);
    file_path.copy_trailing_sep_from(slice);
    let file_path = file_path.to_zbox();

    let (last, found) = restat(&file_path);
    let prev = stat::to_js(global, &last, bigint)?;

    let id = NEXT_ID.get();
    NEXT_ID.set(id.wrapping_add(1).max(1));
    let event_loop: Rc<EventLoop> = EventLoop::of(cx(global));
    let tick = NativeFunction::from_copy_closure(move |_, _, context| {
        let global = JSGlobalObject::from_context(context);
        lift(global, check(global, id)).map(|()| boa_engine::JsValue::undefined())
    });
    let function = FunctionObjectBuilder::new(cx(global).realm(), tick).build();
    let interval = Duration::from_millis(u64::from(interval.max(5).unsigned_abs()));
    let timer = event_loop.add_timer(
        Callback::Function {
            function: function.into(),
            args: Vec::new(),
        },
        interval,
        true,
    );
    if !persistent {
        event_loop.set_timer_ref(timer, false);
    }
    WATCHERS.with_borrow_mut(|watchers| {
        watchers.insert(
            id,
            Watcher {
                path: file_path,
                listener: listener.clone(),
                prev: prev.clone(),
                last,
                bigint,
                timer,
                event_loop: Rc::downgrade(&event_loop),
            },
        )
    });
    if !found {
        // A path that cannot be stat'd: the listener hears of it once, with two zeroed stats.
        event_loop.enqueue(move |context| {
            let global = JSGlobalObject::from_context(context);
            if WATCHERS.with_borrow(|watchers| !watchers.contains_key(&id)) {
                return Ok(());
            }
            lift(global, listener.call(global, JSValue::UNDEFINED, &[prev.clone(), prev]).map(drop))
        });
    }
    bun_js::class::to_js(global, StatWatcher { id })
}
