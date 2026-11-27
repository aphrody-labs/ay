//! `FileSink`, what `Bun.file(...).writer()` answers: src/runtime/webcore/FileSink.rs over its POSIX streaming writer
//! (src/io/PipeWriter.rs). What is written below a page is kept and flushed in the next tick of the event loop, or by
//! `flush()` or `end()`; a write that reaches a page writes all that is kept. A path is opened for writing without
//! truncating it, and closed by `end()`; a descriptor stays open.
//!
//! The writer is synchronous: where the descriptor is full (a pipe), it waits until it can write, and `write()` never
//! answers a promise.

use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};

use boa_gc::{Finalize, Trace};
use bun_js::class::{Attributes, ClassBuilder};
use bun_js::{ErrorCode, JSGlobalObject, JSValue, JsClass, JsResult};
use bun_sys::{E, Fd, FdExt};

use super::blob::{Blob, PathOrFileDescriptor, throw_system_error, validate_writable_blob};
use crate::sys_js::SystemError;
use crate::{EventLoop, cx};

/// `PosixStreamingWriter::CHUNK_SIZE`: the smallest page size of the target.
const CHUNK_SIZE: usize = if cfg!(all(target_os = "macos", target_arch = "aarch64")) { 16384 } else { 4096 };

const WRITE_PERMISSIONS: bun_sys::Mode = 0o664;

thread_local! {
    static NEXT_SINK_ID: Cell<u32> = const { Cell::new(1) };
}

struct Sink {
    fd: Fd,
    /// A path that `writer()` opened: `end()` closes it.
    owns_fd: bool,
    /// What `write()` took that has not reached the descriptor.
    buffer: RefCell<Vec<u8>>,
    done: Cell<bool>,
    /// A flush is queued for the next tick.
    flush_queued: Cell<bool>,
    event_loop: Weak<EventLoop>,
    sink_id: u32,
}

impl Sink {
    /// Writes all of `bytes`, waiting where the descriptor is full.
    fn write_all(&self, mut bytes: &[u8]) -> bun_sys::Result<usize> {
        let total = bytes.len();
        while !bytes.is_empty() {
            match bun_sys::write(self.fd, bytes) {
                Ok(0) => break,
                Ok(written) => bytes = &bytes[written..],
                Err(err) if err.get_errno() == E::EINTR => {}
                Err(err) if err.get_errno() == E::EAGAIN => {
                    let mut poll = libc::pollfd {
                        fd: self.fd.native(),
                        events: libc::POLLOUT,
                        revents: 0,
                    };
                    // SAFETY: one pollfd that lives across the call.
                    unsafe { libc::poll(&mut poll, 1, -1) };
                }
                Err(err) => return Err(err),
            }
        }
        Ok(total - bytes.len())
    }

    /// `PosixStreamingWriter::flush`: what is kept, written; the bytes that were.
    fn flush(&self) -> bun_sys::Result<usize> {
        let pending = std::mem::take(&mut *self.buffer.borrow_mut());
        if pending.is_empty() {
            return Ok(0);
        }
        self.write_all(&pending)
    }

    fn close(&self) {
        if self.owns_fd {
            self.fd.close();
        }
    }

    /// `on_auto_flush`: a write that was kept is flushed in the next tick, which the queued task keeps alive.
    fn queue_flush(self: &Rc<Self>) {
        if self.flush_queued.replace(true) {
            return;
        }
        let Some(event_loop) = self.event_loop.upgrade() else { return };
        let sink = Rc::clone(self);
        event_loop.enqueue(move |_| {
            sink.flush_queued.set(false);
            if !sink.done.get() && sink.flush().is_err() {
                sink.done.set(true);
                sink.close();
            }
            Ok(())
        });
    }
}

impl Drop for Sink {
    fn drop(&mut self) {
        if !self.done.get() {
            let _ = self.flush();
            self.close();
        }
    }
}

#[derive(Trace, Finalize)]
pub(crate) struct FileSink {
    #[unsafe_ignore_trace]
    sink: Rc<Sink>,
}

const METHOD: Attributes = Attributes {
    writable: true,
    enumerable: false,
    configurable: true,
};

fn sys_error(global: &JSGlobalObject, err: bun_sys::Error) -> bun_js::JsError {
    throw_system_error(global, &SystemError::from(err))
}

/// The bytes of a chunk: a string as UTF-8, an `ArrayBufferView` or an `ArrayBuffer` as it is.
fn chunk_bytes(global: &JSGlobalObject, chunk: &JSValue) -> JsResult<Vec<u8>> {
    if chunk.is_string() {
        return Ok(chunk.to_utf8(global)?.into_bytes());
    }
    if let Some(bytes) = chunk.array_buffer_or_view_bytes(global)? {
        return Ok(bytes);
    }
    Err(ErrorCode::INVALID_ARG_TYPE.throw(
        global,
        format_args!("write() expects a string, ArrayBufferView, or ArrayBuffer"),
    ))
}

impl JsClass for FileSink {
    const NAME: &'static str = "FileSink";

    fn define(class: &mut ClassBuilder<'_, Self>) -> JsResult<()> {
        class.method("write", 1, METHOD, |this, global, frame| {
            let sink = Rc::clone(&this.borrow(global)?.sink);
            let bytes = chunk_bytes(global, &frame.argument(0))?;
            if sink.done.get() {
                return Ok(JSValue::js_boolean(true));
            }
            let kept = sink.buffer.borrow().len();
            if kept + bytes.len() < CHUNK_SIZE {
                sink.buffer.borrow_mut().extend_from_slice(&bytes);
                if !bytes.is_empty() {
                    sink.queue_flush();
                }
                return Ok(JSValue::js_number(bytes.len() as f64));
            }
            sink.buffer.borrow_mut().extend_from_slice(&bytes);
            sink.flush().map_err(|err| sys_error(global, err))?;
            Ok(JSValue::js_number(bytes.len() as f64))
        })?;
        class.method("flush", 0, METHOD, |this, global, _| {
            let sink = Rc::clone(&this.borrow(global)?.sink);
            if sink.done.get() {
                return Ok(JSValue::UNDEFINED);
            }
            let flushed = sink.flush().map_err(|err| sys_error(global, err))?;
            Ok(JSValue::js_number(flushed as f64))
        })?;
        class.method("end", 1, METHOD, |this, global, _| {
            let sink = Rc::clone(&this.borrow(global)?.sink);
            if sink.done.replace(true) {
                return Ok(JSValue::js_number(0.0));
            }
            let flushed = sink.flush();
            sink.close();
            let flushed = flushed.map_err(|err| sys_error(global, err))?;
            Ok(JSValue::js_number(flushed as f64))
        })?;
        class.method("close", 0, METHOD, |this, global, _| {
            let sink = Rc::clone(&this.borrow(global)?.sink);
            if !sink.done.replace(true) {
                let flushed = sink.flush();
                sink.close();
                flushed.map_err(|err| sys_error(global, err))?;
            }
            Ok(JSValue::UNDEFINED)
        })?;
        class.method("start", 1, METHOD, |_, _, _| Ok(JSValue::UNDEFINED))?;
        class.method("ref", 0, METHOD, |_, _, _| Ok(JSValue::UNDEFINED))?;
        class.method("unref", 0, METHOD, |_, _, _| Ok(JSValue::UNDEFINED))?;
        class.method("_getFd", 0, METHOD, |this, global, _| {
            Ok(JSValue::js_number(f64::from(this.borrow(global)?.sink.fd.native())))
        })?;
        class.getter("sinkId", METHOD, |this, global| {
            Ok(JSValue::js_number(f64::from(this.borrow(global)?.sink.sink_id)))
        })
    }
}

/// `blob.writer(options)`.
pub(crate) fn writer(global: &JSGlobalObject, blob: &Blob, options: &JSValue) -> JsResult<JSValue> {
    if !options.is_undefined_or_null() && !options.is_object() {
        return Err(ErrorCode::INVALID_ARG_TYPE.throw(global, format_args!("options must be an object or undefined")));
    }
    validate_writable_blob(global, blob)?;
    let file = blob.file().expect("validate_writable_blob: a file");
    let (fd, owns_fd) = match &file.pathlike {
        PathOrFileDescriptor::Fd(fd) => (*fd, false),
        PathOrFileDescriptor::Path(path) => {
            let flags = bun_sys::O::WRONLY | bun_sys::O::CREAT | bun_sys::O::NONBLOCK | bun_sys::O::CLOEXEC;
            match bun_sys::open(&bun_paths::Path::from(path.as_slice()), flags, WRITE_PERMISSIONS) {
                Ok(fd) => (fd, true),
                Err(err) => return Err(sys_error(global, err.with_path(path))),
            }
        }
    };
    let sink_id = NEXT_SINK_ID.get();
    NEXT_SINK_ID.set(sink_id.wrapping_add(1));
    let sink = Rc::new(Sink {
        fd,
        owns_fd,
        buffer: RefCell::default(),
        done: Cell::new(false),
        flush_queued: Cell::new(false),
        event_loop: cx(global)
            .downcast_job_executor::<EventLoop>()
            .map(|event_loop| Rc::downgrade(&event_loop))
            .unwrap_or_default(),
        sink_id,
    });
    bun_js::class::to_js(global, FileSink { sink })
}
