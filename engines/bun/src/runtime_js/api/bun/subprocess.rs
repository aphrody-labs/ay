//! What `Bun.spawn` returns: src/runtime/api/bun/subprocess.rs on `bun_js`, with its pipes and its exit watched on
//! the realm's event loop (the child's pidfd and the parent's ends of the pipes, registered with the reactor).
//!
//! As in the JavaScriptCore build: `exited` resolves with the exit code (128 plus the signal of a child that a
//! signal ended), `onExit(subprocess, exitCode, signalCode, error)` runs in a later task, `killed` is true once the
//! child ended, `kill()` defaults to SIGTERM, and a ref'd child that runs, or whose pipes are open, keeps the
//! process alive. A piped `stdout` or `stderr` is a `ReadableStream` of `Uint8Array`s, made at spawn.
//!
//! What differs: Boa has no `FileSink` yet. A piped `stdin` is an object with `write()`, `flush()` and `end()`.
//! Cancelling a piped `stdout` or `stderr` does not close the pipe.

use core::ffi::c_int;
use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};
use std::time::{Duration, Instant};

use boa_engine::object::builtins::JsPromise;
use boa_engine::{JsObject, JsValue};
use boa_gc::{Finalize, Trace};
use bun_http_rs::reactor::{Interest, Io, Reactor, Readiness, TimerId, Token};
use bun_js::class::{Attributes, ClassBuilder, ClassRef};
use bun_js::{JSGlobalObject, JSValue, JsClass, JsResult};
use bun_spawn_sys::Rusage;
use bun_sys::File;

use super::js_bun_spawn_bindings::{
    SpawnArgs, Status, Stdio, exit_to_js, read_available, reap, signal_from_js, start, write_available,
};
use super::resource_usage::ResourceUsage;
use crate::event_loop::EventLoop;
use crate::web::streams::readable_stream::{self, stream_of};
use crate::web::streams::readable_stream_default_controller::{self as controller, SourceAlgorithms};
use crate::{cx, lift};

/// What `stdout`, `stderr` or `stdin` is to JavaScript.
#[derive(Clone, Copy)]
enum Shown {
    Undefined,
    Fd(c_int),
    Pipe,
}

impl Shown {
    fn of(stdio: &Stdio) -> Shown {
        match stdio {
            Stdio::Pipe => Shown::Pipe,
            Stdio::Fd(fd) => Shown::Fd(fd.native()),
            _ => Shown::Undefined,
        }
    }
}

/// The parent's end of a pipe the child writes to.
struct Output {
    shown: Shown,
    file: RefCell<Option<(File, Token)>>,
    /// The controller of the stream that `stdout` or `stderr` is, until the pipe closed.
    controller: RefCell<Option<JSValue>>,
    /// What the child wrote so far, for `maxBuffer`.
    read: Cell<usize>,
}

/// The parent's end of the child's stdin.
struct Input {
    shown: Shown,
    file: RefCell<Option<(File, Token)>>,
    pending: RefCell<Vec<u8>>,
    written: Cell<usize>,
    /// `end()` was called, or the bytes it was given are all it gets: it closes once they are written.
    ending: Cell<bool>,
    flush_scheduled: Cell<bool>,
}

pub(crate) struct State {
    pid: c_int,
    event_loop: Rc<EventLoop>,
    kill_signal: c_int,
    max_buffer: Option<i64>,
    status: Cell<Option<Status>>,
    rusage: Cell<Rusage>,
    exited_due_to_max_buffer: Cell<bool>,
    pidfd: RefCell<Option<(File, Token)>>,
    stdout: Output,
    stderr: Output,
    stdin: Input,
    extra: Vec<Option<c_int>>,
    timeout: Cell<Option<TimerId>>,
    /// `ref()`/`unref()`.
    refed: Cell<bool>,
    /// Whether this child holds a ref of the reactor now.
    holds_ref: Cell<bool>,
    exited: RefCell<Option<JsObject>>,
    resolve_exited: RefCell<Option<JsObject>>,
    on_exit: RefCell<Option<JsObject>>,
    /// The `Subprocess` object, for `onExit`, until the child ended.
    this: RefCell<Option<JSValue>>,
}

#[derive(Clone, Copy)]
enum Which {
    Exit,
    Stdout,
    Stderr,
    Stdin,
}

/// What the reactor calls when one of the child's descriptors is ready.
struct Watch {
    state: Weak<State>,
    which: Which,
}

impl Io for Watch {
    fn ready(self: Rc<Self>, _: &Rc<Reactor>, readiness: Readiness) {
        let Some(state) = self.state.upgrade() else { return };
        match self.which {
            Which::Exit => state.on_exit_ready(),
            Which::Stdout => state.on_output_ready(&state.stdout),
            Which::Stderr => state.on_output_ready(&state.stderr),
            Which::Stdin => {
                if readiness.writable || readiness.error || readiness.write_closed {
                    state.flush_stdin();
                }
            }
        }
    }
}

fn register(state: &Rc<State>, file: &File, which: Which, interest: Interest) -> Option<Token> {
    let raw = file.handle().native();
    let watch = Rc::new(Watch {
        state: Rc::downgrade(state),
        which,
    });
    state
        .event_loop
        .reactor()
        .register(&mut mio::unix::SourceFd(&raw), interest, watch)
        .ok()
}

fn deregister(state: &State, slot: &RefCell<Option<(File, Token)>>) {
    if let Some((file, token)) = slot.borrow_mut().take() {
        let raw = file.handle().native();
        state.event_loop.reactor().deregister(&mut mio::unix::SourceFd(&raw), token);
    }
}

impl State {
    fn has_exited(&self) -> bool {
        self.status.get().is_some()
    }

    /// Holds a ref of the reactor while a ref'd child runs, or while one of its pipes is open.
    fn update_ref(&self) {
        let reading = |output: &Output| output.file.borrow().is_some();
        let wanted = self.refed.get() && (!self.has_exited() || reading(&self.stdout) || reading(&self.stderr));
        if wanted != self.holds_ref.get() {
            self.holds_ref.set(wanted);
            if wanted {
                self.event_loop.reactor().ref_();
            } else {
                self.event_loop.reactor().unref();
            }
        }
    }

    fn kill(&self, signal: c_int) {
        if self.has_exited() {
            return;
        }
        // SAFETY: kill(2) takes integers; ESRCH (a child that just ended) is not an error here.
        unsafe { libc::kill(self.pid, signal) };
    }

    fn on_output_ready(self: &Rc<Self>, output: &Output) {
        let Some(fd) = output.file.borrow().as_ref().map(|(file, _)| file.handle()) else { return };
        let mut bytes = Vec::new();
        let ended = read_available(fd, &mut bytes);
        output.read.set(output.read.get() + bytes.len());
        if let Some(max) = self.max_buffer
            && output.read.get() as i64 > max
            && !self.exited_due_to_max_buffer.get()
        {
            self.exited_due_to_max_buffer.set(true);
            self.kill(self.kill_signal);
        }
        if !bytes.is_empty() {
            self.deliver(output, Some(bytes));
        }
        if ended {
            deregister(self, &output.file);
            self.deliver(output, None);
            self.update_ref();
        }
    }

    /// Enqueues `bytes` to the stream of `output` in a task, or closes it for `None`.
    fn deliver(&self, output: &Output, bytes: Option<Vec<u8>>) {
        let controller = match &bytes {
            Some(_) => output.controller.borrow().clone(),
            None => output.controller.borrow_mut().take(),
        };
        let Some(controller) = controller else { return };
        self.event_loop.enqueue(move |context| {
            let global = JSGlobalObject::from_context(context);
            lift(global, (|| {
                if !controller::can_close_or_enqueue(global, &controller)? {
                    return Ok(());
                }
                match bytes {
                    Some(bytes) => controller::enqueue(global, &controller, crate::engine::uint8_array(global, &bytes)?),
                    None => controller::close(global, &controller),
                }
            })())
        });
    }

    /// Writes what the pipe takes of what is pending; closes it once all is written after `end()`. The number of
    /// bytes this wrote.
    fn flush_stdin(self: &Rc<Self>) -> usize {
        let Some(fd) = self.stdin.file.borrow().as_ref().map(|(file, _)| file.handle()) else { return 0 };
        let before = self.stdin.written.get();
        let mut written = before;
        let all = write_available(fd, &self.stdin.pending.borrow(), &mut written);
        self.stdin.written.set(written);
        if all {
            let gone = written < self.stdin.pending.borrow().len();
            self.stdin.pending.borrow_mut().clear();
            self.stdin.written.set(0);
            if self.stdin.ending.get() || gone {
                deregister(self, &self.stdin.file);
            }
        }
        written - before
    }

    /// As a `FileSink` does, what `write()` buffered goes out in the next task unless `flush()` sends it first.
    fn schedule_flush(self: &Rc<Self>) {
        if self.stdin.flush_scheduled.replace(true) {
            return;
        }
        let state = self.clone();
        self.event_loop.enqueue(move |_| {
            state.stdin.flush_scheduled.set(false);
            state.flush_stdin();
            Ok(())
        });
    }

    fn on_exit_ready(self: &Rc<Self>) {
        let mut rusage = self.rusage.get();
        let Some(status) = reap(self.pid, false, &mut rusage) else { return };
        self.rusage.set(rusage);
        self.finish(status);
    }

    fn finish(self: &Rc<Self>, status: Status) {
        self.status.set(Some(status));
        deregister(self, &self.pidfd);
        if let Some(timer) = self.timeout.take() {
            self.event_loop.reactor().cancel_timer(timer);
        }
        let state = self.clone();
        self.event_loop.enqueue(move |context| {
            let code = match status {
                Status::Exited(code) => code,
                Status::Signaled(signal) => 128 + signal,
            };
            if let Some(resolve) = state.resolve_exited.borrow_mut().take() {
                resolve.call(&JsValue::undefined(), &[code.into()], context)?;
            }
            let after = state.clone();
            state.event_loop.enqueue(move |context| {
                let this = after.this.borrow_mut().take();
                let Some(on_exit) = after.on_exit.borrow_mut().take() else { return Ok(()) };
                let (exit_code, signal_code) = exit_to_js(&status);
                let this = this.map_or(JsValue::undefined(), JSValue::into_engine);
                on_exit.call(
                    &JsValue::undefined(),
                    &[this, exit_code.into_engine(), signal_code.into_engine(), JsValue::undefined()],
                    context,
                )?;
                Ok(())
            });
            Ok(())
        });
        self.update_ref();
    }
}

/// `Bun.spawn`: the child, and the `Subprocess` that watches it.
pub(crate) fn spawn(global: &JSGlobalObject, args: SpawnArgs) -> JsResult<JSValue> {
    let mut spawned = start(global, &args)?;
    let event_loop = EventLoop::of(cx(global));
    let (promise, resolvers) = JsPromise::new_pending(cx(global));
    let stdin_bytes = match &args.stdio[0] {
        Stdio::Bytes(bytes) => Some(bytes.clone()),
        _ => None,
    };
    let (stdout, stdout_stream) = output(global, &args.stdio[1])?;
    let (stderr, stderr_stream) = output(global, &args.stdio[2])?;
    let state = Rc::new(State {
        pid: spawned.pid,
        event_loop,
        kill_signal: args.kill_signal,
        max_buffer: args.max_buffer,
        status: Cell::new(None),
        rusage: Cell::new(bun_spawn_sys::rusage_zeroed()),
        exited_due_to_max_buffer: Cell::new(false),
        pidfd: RefCell::new(None),
        stdout,
        stderr,
        stdin: Input {
            shown: Shown::of(&args.stdio[0]),
            file: RefCell::new(None),
            pending: RefCell::new(stdin_bytes.clone().unwrap_or_default()),
            written: Cell::new(0),
            ending: Cell::new(stdin_bytes.is_some()),
            flush_scheduled: Cell::new(false),
        },
        extra: args
            .extra
            .iter()
            .map(|stdio| match stdio {
                Stdio::Fd(fd) => Some(fd.native()),
                _ => None,
            })
            .collect(),
        timeout: Cell::new(None),
        refed: Cell::new(true),
        holds_ref: Cell::new(false),
        exited: RefCell::new(Some(promise.into())),
        resolve_exited: RefCell::new(Some(resolvers.resolve.into())),
        on_exit: RefCell::new(args.on_exit.and_then(|f| f.into_engine().as_object())),
        this: RefCell::new(None),
    });
    for (file, which) in [(spawned.stdout.take(), Which::Stdout), (spawned.stderr.take(), Which::Stderr)] {
        let Some(file) = file else { continue };
        let slot = match which {
            Which::Stdout => &state.stdout,
            _ => &state.stderr,
        };
        match register(&state, &file, which, Interest::READABLE) {
            Some(token) => *slot.file.borrow_mut() = Some((file, token)),
            None => state.deliver(slot, None),
        }
    }
    if let Some(file) = spawned.stdin.take()
        && let Some(token) = register(&state, &file, Which::Stdin, Interest::WRITABLE)
    {
        *state.stdin.file.borrow_mut() = Some((file, token));
        state.flush_stdin();
    }
    let this = bun_js::class::to_js(
        global,
        Subprocess {
            state: state.clone(),
            stdout: stdout_stream,
            stderr: stderr_stream,
            stdin: None,
        },
    )?;
    *state.this.borrow_mut() = Some(this.clone());
    match spawned.pidfd.take() {
        Some(pidfd) => match register(&state, &pidfd, Which::Exit, Interest::READABLE) {
            Some(token) => *state.pidfd.borrow_mut() = Some((pidfd, token)),
            None => state.on_exit_ready(),
        },
        // Gone before a pidfd was asked for: it is reaped now.
        None => {
            let mut rusage = state.rusage.get();
            let status = reap(state.pid, true, &mut rusage).unwrap_or(Status::Exited(0));
            state.rusage.set(rusage);
            state.finish(status);
        }
    }
    if let Some(ms) = args.timeout
        && !state.has_exited()
    {
        let weak = Rc::downgrade(&state);
        let timer = state.event_loop.reactor().set_timer(Instant::now() + Duration::from_millis(u64::from(ms)), move |_| {
            if let Some(state) = weak.upgrade() {
                state.timeout.set(None);
                state.kill(state.kill_signal);
            }
        });
        state.timeout.set(Some(timer));
    }
    state.update_ref();
    Ok(this)
}

/// The watch of a pipe the child writes to, and its stream if it is piped.
fn output(global: &JSGlobalObject, stdio: &Stdio) -> JsResult<(Output, Option<JSValue>)> {
    let shown = Shown::of(stdio);
    let stream = match shown {
        Shown::Pipe => Some(readable_stream::create(global, SourceAlgorithms::None, 0.0)?),
        _ => None,
    };
    let controller = match &stream {
        Some(stream) => Some(stream_of(stream).borrow(global)?.controller.clone()),
        None => None,
    };
    let output = Output {
        shown,
        file: RefCell::new(None),
        controller: RefCell::new(controller),
        read: Cell::new(0),
    };
    Ok((output, stream))
}

#[derive(Trace, Finalize)]
pub(crate) struct Subprocess {
    #[unsafe_ignore_trace]
    state: Rc<State>,
    /// The streams of a piped `stdout` and `stderr`, and the object of a piped `stdin`, made the first time it is
    /// read (the schema's `cache`). They are the payload's, which the collector traces.
    stdout: Option<JSValue>,
    stderr: Option<JSValue>,
    stdin: Option<JSValue>,
}

fn state_of(this: &ClassRef<Subprocess>, global: &JSGlobalObject) -> JsResult<Rc<State>> {
    Ok(this.borrow(global)?.state.clone())
}

/// The object of a pipe, made once.
fn cached(
    this: &ClassRef<Subprocess>,
    global: &JSGlobalObject,
    slot: fn(&mut Subprocess) -> &mut Option<JSValue>,
    make: impl FnOnce(Rc<State>) -> JsResult<JSValue>,
) -> JsResult<JSValue> {
    if let Some(object) = slot(&mut *this.borrow_mut(global)?) {
        return Ok(object.clone());
    }
    let object = make(state_of(this, global)?)?;
    *slot(&mut *this.borrow_mut(global)?) = Some(object.clone());
    Ok(object)
}

/// What `stdout` and `stderr` are: a number, `undefined`, or the object of a pipe.
fn output_value(this: &ClassRef<Subprocess>, global: &JSGlobalObject, which: Which) -> JsResult<JSValue> {
    let state = state_of(this, global)?;
    let shown = match which {
        Which::Stdout => state.stdout.shown,
        _ => state.stderr.shown,
    };
    match shown {
        Shown::Undefined => Ok(JSValue::UNDEFINED),
        Shown::Fd(fd) => Ok(JSValue::js_number_from_int32(fd)),
        Shown::Pipe => {
            let this = this.borrow(global)?;
            let stream = match which {
                Which::Stdout => &this.stdout,
                _ => &this.stderr,
            };
            Ok(stream.clone().unwrap_or(JSValue::UNDEFINED))
        }
    }
}

fn input_value(this: &ClassRef<Subprocess>, global: &JSGlobalObject) -> JsResult<JSValue> {
    match state_of(this, global)?.stdin.shown {
        Shown::Undefined => Ok(JSValue::UNDEFINED),
        Shown::Fd(fd) => Ok(JSValue::js_number_from_int32(fd)),
        Shown::Pipe => cached(this, global, |this| &mut this.stdin, |state| {
            bun_js::class::to_js(global, PipeWritable { state })
        }),
    }
}

const MEMBER: Attributes = Attributes::DEFAULT;

impl JsClass for Subprocess {
    const NAME: &'static str = "Subprocess";

    fn define(class: &mut ClassBuilder<'_, Self>) -> JsResult<()> {
        class.getter("connected", MEMBER, |_, _| Ok(JSValue::js_boolean(false)))?;
        class.method("disconnect", 0, MEMBER, |_, _, _| Ok(JSValue::UNDEFINED))?;
        class.getter("exitCode", MEMBER, |this, global| {
            Ok(match state_of(this, global)?.status.get() {
                Some(Status::Exited(code)) => JSValue::js_number_from_int32(code),
                _ => JSValue::NULL,
            })
        })?;
        class.getter("exited", MEMBER, |this, global| {
            let state = state_of(this, global)?;
            let promise = state.exited.borrow().clone();
            Ok(promise.map_or(JSValue::UNDEFINED, |promise| JSValue::from_engine(promise.into())))
        })?;
        class.method("kill", 1, MEMBER, |this, global, frame| {
            let state = state_of(this, global)?;
            let signal = signal_from_js(global, &frame.argument(0))?;
            state.kill(signal);
            Ok(JSValue::UNDEFINED)
        })?;
        class.getter("killed", MEMBER, |this, global| Ok(JSValue::js_boolean(state_of(this, global)?.has_exited())))?;
        class.getter("pid", MEMBER, |this, global| Ok(JSValue::js_number_from_int32(state_of(this, global)?.pid)))?;
        class.getter("readable", MEMBER, |this, global| output_value(this, global, Which::Stdout))?;
        class.method("ref", 0, MEMBER, |this, global, _| {
            let state = state_of(this, global)?;
            state.refed.set(true);
            state.update_ref();
            Ok(JSValue::UNDEFINED)
        })?;
        class.method("resourceUsage", 0, MEMBER, |this, global, _| {
            let state = state_of(this, global)?;
            if !state.has_exited() {
                return Ok(JSValue::UNDEFINED);
            }
            bun_js::class::to_js(global, ResourceUsage::new(state.rusage.get()))
        })?;
        class.method("send", 1, MEMBER, |_, global, _| Err(global.throw(format_args!("IPC is not implemented on Boa yet"))))?;
        class.getter("signalCode", MEMBER, |this, global| {
            Ok(match state_of(this, global)?.status.get() {
                Some(status) => exit_to_js(&status).1,
                None => JSValue::NULL,
            })
        })?;
        class.getter("stderr", MEMBER, |this, global| output_value(this, global, Which::Stderr))?;
        class.getter("stdin", MEMBER, input_value)?;
        class.getter("stdio", MEMBER, |this, global| {
            let state = state_of(this, global)?;
            let mut items = vec![JSValue::NULL, JSValue::NULL, JSValue::NULL];
            items.extend(state.extra.iter().map(|fd| fd.map_or(JSValue::NULL, JSValue::js_number_from_int32)));
            JSValue::create_array_from_slice(global, &items)
        })?;
        class.getter("stdout", MEMBER, |this, global| output_value(this, global, Which::Stdout))?;
        class.getter("terminal", MEMBER, |_, _| Ok(JSValue::UNDEFINED))?;
        class.method("unref", 0, MEMBER, |this, global, _| {
            let state = state_of(this, global)?;
            state.refed.set(false);
            state.update_ref();
            Ok(JSValue::UNDEFINED)
        })?;
        class.getter("writable", MEMBER, input_value)
    }
}

/// A piped `stdin`: what `FileSink` has of writing.
#[derive(Trace, Finalize)]
pub(crate) struct PipeWritable {
    #[unsafe_ignore_trace]
    state: Rc<State>,
}

fn chunk_bytes(global: &JSGlobalObject, chunk: &JSValue) -> JsResult<Vec<u8>> {
    if let Some(text) = chunk.as_string() {
        return Ok(text.into_bytes());
    }
    if let Some(bytes) = crate::webcore::body::buffer_source_bytes(chunk.as_engine(), cx(global)) {
        return Ok(bytes);
    }
    Err(bun_js::ErrorCode::INVALID_ARG_TYPE.throw(
        global,
        format_args!("write() expects a string, ArrayBufferView, or ArrayBuffer"),
    ))
}

impl JsClass for PipeWritable {
    const NAME: &'static str = "FileSink";

    fn define(class: &mut ClassBuilder<'_, Self>) -> JsResult<()> {
        let method = Attributes {
            writable: true,
            enumerable: true,
            configurable: true,
        };
        // `write()` buffers and answers the length of the chunk; `flush()` and `end()` answer what they sent.
        class.method("write", 1, method, |this, global, frame| {
            let state = this.borrow(global)?.state.clone();
            let bytes = chunk_bytes(global, &frame.argument(0))?;
            let length = bytes.len();
            if state.stdin.file.borrow().is_some() && !state.stdin.ending.get() {
                state.stdin.pending.borrow_mut().extend_from_slice(&bytes);
                state.schedule_flush();
            }
            Ok(JSValue::js_number(length as f64))
        })?;
        class.method("flush", 0, method, |this, global, _| {
            let state = this.borrow(global)?.state.clone();
            Ok(JSValue::js_number(state.flush_stdin() as f64))
        })?;
        class.method("end", 0, method, |this, global, _| {
            let state = this.borrow(global)?.state.clone();
            state.stdin.ending.set(true);
            let sent = state.flush_stdin();
            if state.stdin.pending.borrow().is_empty() {
                deregister(&state, &state.stdin.file);
            }
            Ok(JSValue::js_number(sent as f64))
        })
    }
}
