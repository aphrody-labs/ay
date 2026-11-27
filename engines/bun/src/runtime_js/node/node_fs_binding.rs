//! The bodies of src/runtime/node/node_fs_binding.bind.ts on `bun_js`: src/runtime/node/node_fs_binding.rs. `x` returns
//! a promise, `xCb` calls back, `xSync` returns or throws; the async forms run the operation on the work pool and
//! settle on the JavaScript thread (`JSGlobalObject::run_on_work_pool`).

use boa_gc::{Finalize, Trace};
use bun_js::bindgen::JsScope;
use bun_js::{JSGlobalObject, JSPromise, JSValue, JsResult};

use crate::EventLoop;
use crate::generated_bindgen::node_fs_binding::RmOptions;
use crate::node::node_fs::{self as fs, Operation};
use crate::node::stat;
use crate::node::types::{ArgumentsSlice, invalid_arguments, sys_error_to_js};

/// The default export of src/js/internal/fs/binding.ts. It holds nothing: every call parses its own arguments.
#[derive(Trace, Finalize)]
pub struct Binding;

/// `Bindings(FunctionEnum).runSync`.
fn run_sync<O: Operation>(global: &JSGlobalObject, args: &[JSValue]) -> JsResult<JSValue> {
    let mut slice = ArgumentsSlice::new(args, false);
    let (op, keep) = O::from_js(global, &mut slice)?;
    match op.run() {
        Ok(out) => op.to_js(global, out, keep),
        Err(err) => Err(global.throw_value(sys_error_to_js(global, &err))),
    }
}

/// An error found before anything is scheduled: the promise is rejected, or the callback gets it on the next tick.
fn reject_before_schedule(global: &JSGlobalObject, callback: Option<JSValue>, error: JSValue) -> JsResult<JSValue> {
    match callback {
        None => Ok(JSPromise::rejected_promise_value(global, error)),
        Some(callback) => {
            let next_tick = match global.to_js_value().get(global, "process")? {
                Some(process) => process.get(global, "nextTick")?,
                None => None,
            };
            match next_tick.filter(JSValue::is_callable) {
                Some(next_tick) => next_tick.call(global, JSValue::UNDEFINED, &[callback, error]).map(drop)?,
                None => callback.call(global, JSValue::UNDEFINED, &[error]).map(drop)?,
            }
            Ok(JSValue::UNDEFINED)
        }
    }
}

/// `Bindings(FunctionEnum).runAsync`: a promise without `callback`, `undefined` with it.
fn run_async<O: Operation>(global: &JSGlobalObject, args: &[JSValue], with_callback: bool) -> JsResult<JSValue> {
    let (callback, args) = if with_callback {
        match args.split_first() {
            Some((callback, rest)) if callback.is_callable() => (Some(callback.clone()), rest),
            _ => return Err(invalid_arguments(global, format_args!("callback must be a function"))),
        }
    } else {
        (None, args)
    };
    let mut slice = ArgumentsSlice::new(args, true);
    let (op, keep) = match O::from_js(global, &mut slice) {
        Ok(parsed) => parsed,
        // An aborted signal is reported, not thrown: what node's `checkAborted` does.
        Err(error) if is_abort_error(global) => {
            let error = global.take_exception(error);
            return reject_before_schedule(global, callback, error);
        }
        Err(error) => return Err(error),
    };
    if let Some(err) = slice.deferred_error.take() {
        return reject_before_schedule(global, callback, sys_error_to_js(global, &err));
    }
    let promise = callback.is_none().then(|| JSPromise::create(global));
    let value = promise.as_ref().map_or(JSValue::UNDEFINED, |p| p.promise());
    EventLoop::of(crate::cx(global)).run_on_work_pool(
        move || {
            let out = op.run();
            (op, out)
        },
        move |context, (op, out)| {
            let global = JSGlobalObject::from_context(context);
            let settled = match out {
                Ok(out) => op.to_js(global, out, keep).map_err(|error| global.take_exception(error)),
                Err(err) => Err(sys_error_to_js(global, &err)),
            };
            let result = match (promise, callback) {
                (Some(promise), _) => match settled {
                    Ok(value) => promise.resolve(global, value),
                    Err(error) => promise.reject(global, error),
                },
                (None, Some(callback)) => {
                    let args = match settled {
                        Ok(value) => vec![JSValue::NULL, value],
                        Err(error) => vec![error],
                    };
                    callback.call(global, JSValue::UNDEFINED, &args).map(drop)
                }
                (None, None) => Ok(()),
            };
            crate::lift(global, result)
        },
    );
    Ok(value)
}

fn is_abort_error(global: &JSGlobalObject) -> bool {
    global
        .peek_exception()
        .and_then(|error| error.get(global, "name").ok().flatten())
        .and_then(|name| name.as_string())
        .is_some_and(|name| name == "AbortError")
}

/// The sync, promise and callback member of each operation.
macro_rules! node_fs_bindings {
    ( $( $sync:ident / $promise:ident / $callback:ident => $Op:ty ; )* ) => {
        impl Binding {
            $(
                pub(crate) fn $sync(&self, cx: &mut JsScope<'_>, args: &[JSValue]) -> JsResult<JSValue> {
                    run_sync::<$Op>(cx.global(), args)
                }
                pub(crate) fn $promise(&self, cx: &mut JsScope<'_>, args: &[JSValue]) -> JsResult<JSValue> {
                    run_async::<$Op>(cx.global(), args, false)
                }
                pub(crate) fn $callback(&self, cx: &mut JsScope<'_>, args: &[JSValue]) -> JsResult<JSValue> {
                    run_async::<$Op>(cx.global(), args, true)
                }
            )*
        }
    };
}

#[rustfmt::skip]
node_fs_bindings! {
    access_sync          / access          / access_cb          => fs::Access;
    append_file_sync     / append_file     / append_file_cb     => fs::WriteFileOp<true>;
    close_sync           / close           / close_cb           => fs::FdOp<{ fs::CLOSE }>;
    copy_file_sync       / copy_file       / copy_file_cb       => fs::CopyFile;
    exists_sync          / exists          / exists_cb          => fs::Exists;
    chown_sync           / chown           / chown_cb           => fs::ChownOp<true>;
    chmod_sync           / chmod           / chmod_cb           => fs::ChmodOp<true>;
    fchmod_sync          / fchmod          / fchmod_cb          => fs::Fchmod;
    fchown_sync          / fchown          / fchown_cb          => fs::Fchown;
    fstat_sync           / fstat           / fstat_cb           => fs::Fstat;
    fsync_sync           / fsync           / fsync_cb           => fs::FdOp<{ fs::FSYNC }>;
    ftruncate_sync       / ftruncate       / ftruncate_cb       => fs::Ftruncate;
    futimes_sync         / futimes         / futimes_cb         => fs::Futimes;
    lchmod_sync          / lchmod          / lchmod_cb          => fs::ChmodOp<false>;
    lchown_sync          / lchown          / lchown_cb          => fs::ChownOp<false>;
    link_sync            / link            / link_cb            => fs::Link;
    lstat_sync           / lstat           / lstat_cb           => fs::StatOp<false>;
    mkdir_sync           / mkdir           / mkdir_cb           => fs::Mkdir;
    mkdtemp_sync         / mkdtemp         / mkdtemp_cb         => fs::Mkdtemp;
    open_sync            / open            / open_cb            => fs::Open;
    read_sync            / read            / read_cb            => fs::Read;
    write_sync           / write           / write_cb           => fs::Write;
    read_file_sync       / read_file       / read_file_cb       => fs::ReadFile;
    write_file_sync      / write_file      / write_file_cb      => fs::WriteFileOp<false>;
    readlink_sync        / readlink        / readlink_cb        => fs::Readlink;
    readdir_sync         / readdir         / readdir_cb         => fs::Readdir;
    rm_sync              / rm              / rm_cb              => fs::Rm;
    rmdir_sync           / rmdir           / rmdir_cb           => fs::RmdirOp;
    realpath_sync        / realpath        / realpath_cb        => fs::Realpath;
    realpath_native_sync / realpath_native / realpath_native_cb => fs::Realpath;
    rename_sync          / rename          / rename_cb          => fs::Rename;
    stat_sync            / stat            / stat_cb            => fs::StatOp<true>;
    statfs_sync          / statfs          / statfs_cb          => fs::StatFs;
    symlink_sync         / symlink         / symlink_cb         => fs::Symlink;
    truncate_sync        / truncate        / truncate_cb        => fs::Truncate;
    unlink_sync          / unlink          / unlink_cb          => fs::Unlink;
    utimes_sync          / utimes          / utimes_cb          => fs::UtimesOp<true>;
    lutimes_sync         / lutimes         / lutimes_cb         => fs::UtimesOp<false>;
    writev_sync          / writev          / writev_cb          => fs::Writev;
    readv_sync           / readv           / readv_cb           => fs::Readv;
    fdatasync_sync       / fdatasync       / fdatasync_cb       => fs::FdOp<{ fs::FDATASYNC }>;
}

impl Binding {
    pub(crate) fn get_dirent(&self, cx: &mut JsScope<'_>) -> JSValue {
        stat::constructor(cx.global(), "Dirent")
    }

    pub(crate) fn get_stats(&self, cx: &mut JsScope<'_>) -> JSValue {
        stat::constructor(cx.global(), "Stats")
    }

    pub(crate) fn cp(&self, cx: &mut JsScope<'_>, args: &[JSValue]) -> JsResult<JSValue> {
        run_async::<fs::Cp>(cx.global(), args, false)
    }

    pub(crate) fn cp_sync(&self, cx: &mut JsScope<'_>, args: &[JSValue]) -> JsResult<JSValue> {
        run_sync::<fs::Cp>(cx.global(), args)
    }

    pub(crate) fn watch(&self, cx: &mut JsScope<'_>, args: &[JSValue]) -> JsResult<JSValue> {
        crate::node::node_fs_watcher::watch(cx.global(), args)
    }

    pub(crate) fn watch_file(&self, cx: &mut JsScope<'_>, args: &[JSValue]) -> JsResult<JSValue> {
        crate::node::node_fs_stat_watcher::watch_file(cx.global(), args)
    }
}

pub(crate) fn binding(_: &mut JsScope<'_>) -> Binding {
    Binding
}

macro_rules! sync_functions {
    ( $( $name:ident => $Op:ty ; )* ) => {
        $(
            pub(crate) fn $name(cx: &mut JsScope<'_>, args: &[JSValue]) -> JsResult<JSValue> {
                run_sync::<$Op>(cx.global(), args)
            }
        )*
    };
}

sync_functions! {
    append_file_sync => fs::WriteFileOp<true>;
    chmod_sync       => fs::ChmodOp<true>;
    close_sync       => fs::FdOp<{ fs::CLOSE }>;
    exists_sync      => fs::Exists;
    fstat_sync       => fs::Fstat;
    lstat_sync       => fs::StatOp<false>;
    readdir_sync     => fs::Readdir;
    read_file_sync   => fs::ReadFile;
    stat_sync        => fs::StatOp<true>;
}

pub(crate) fn windows_handle_to_fd(cx: &mut JsScope<'_>, _: bun_js::bindgen::JsLocal<'_>) -> JsResult<i32> {
    Err(cx.global().throw(format_args!("windowsHandle is not implemented on this platform")))
}

/// `(path, options)` as `fs.rm` parses them.
pub(crate) fn rm_options_for_testing(cx: &mut JsScope<'_>, args: &[JSValue]) -> JsResult<RmOptions> {
    let global = cx.global();
    let mut slice = ArgumentsSlice::new(args, false);
    let (rm, ()) = fs::Rm::from_js(global, &mut slice)?;
    let (retry_delay, max_retries, recursive, force) = rm.options();
    Ok(RmOptions {
        retry_delay,
        max_retries,
        recursive,
        force,
    })
}

pub(crate) fn string_to_flags_for_testing(cx: &mut JsScope<'_>, flags: bun_js::bindgen::JsLocal<'_>) -> JsResult<i32> {
    let global = cx.global();
    Ok(crate::node::types::flags_from_js(global, flags.value())?.unwrap_or(bun_sys::O::RDONLY))
}

pub(crate) fn create_memfd_for_testing(cx: &mut JsScope<'_>, size: Option<f64>) -> JsResult<Option<i32>> {
    let global = cx.global();
    let Some(size) = size else {
        return Ok(None);
    };
    #[cfg(any(target_os = "linux", target_os = "android"))]
    {
        match bun_sys::memfd_create(c"my_memfd".into(), bun_sys::MemfdFlags::NonExecutable) {
            Ok(fd) => {
                let _ = bun_sys::ftruncate(fd, size as i64);
                Ok(Some(fd.native()))
            }
            Err(err) => Err(global.throw_value(sys_error_to_js(global, &err))),
        }
    }
    #[cfg(not(any(target_os = "linux", target_os = "android")))]
    {
        let _ = size;
        Err(global.throw(format_args!("memfd_create is not implemented on this platform")))
    }
}
