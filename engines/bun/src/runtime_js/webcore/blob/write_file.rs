//! `Bun.write()` and `blob.write()`: `write_file_internal` and what it calls in src/runtime/webcore/Blob.rs, with
//! the writes of blob/write_file.rs and the copies of blob/copy_file.rs, run when they are asked for.

use bun_js::{CallFrame, ErrorCode, JSGlobalObject, JSValue, JsResult};
use bun_paths::Path;
use bun_sys::{E, Fd, Mode, O};

use super::store::{self, Data, Opened, PathOrFileDescriptor, creating_parents, mkdirp_parent};
use super::{Blob, path_or_fd_from_js, validate_writable_blob};
use crate::engine;
use crate::sys_js::SystemError;

/// What `Bun.write` writes to: a path or a descriptor, or a file `Blob`.
pub(crate) enum Destination {
    Path(PathOrFileDescriptor),
    Blob(Blob),
}

#[derive(Default, Clone, Copy)]
pub(crate) struct WriteFileOptions {
    pub(crate) mkdirp_if_not_exists: Option<bool>,
    pub(crate) mode: Option<Mode>,
}

/// What a short write to a path opens a new file with.
const WRITE_PERMISSIONS: Mode = 0o664;
/// node:fs's `DEFAULT_PERMISSION`.
const DEFAULT_PERMISSION: Mode = 0o666;
/// Below this, a string or a buffer is written without making a Blob of it first.
const FAST_PATH_LIMIT: u64 = 256 * 1024;

fn reject(global: &JSGlobalObject, error: impl Into<SystemError>) -> JsResult<JSValue> {
    engine::rejected(global, error.into().to_error_instance(global))
}

/// `Bun.write(destination, input, options?)`.
pub fn write(global: &JSGlobalObject, frame: &CallFrame<'_>) -> JsResult<JSValue> {
    let [destination, data, options] = frame.arguments_as_array::<3>();
    let destination = destination_from_js(global, &destination)?;
    let options = write_options(global, &options)?;
    write_file_internal(global, destination, &data, options)
}

/// `write_destination_from_js`: a path, a file descriptor, or a `Bun.file()`.
fn destination_from_js(global: &JSGlobalObject, value: &JSValue) -> JsResult<Destination> {
    if let Some(path) = path_or_fd_from_js(global, value)? {
        return Ok(Destination::Path(path));
    }
    if let Some(blob) = value.as_class_ref::<Blob>() {
        let blob = blob.borrow(global)?.clone();
        validate_writable_blob(global, &blob)?;
        return Ok(Destination::Blob(blob));
    }
    let received = bun_js::bindgen::determine_specific_type(global, value)?;
    Err(ErrorCode::INVALID_ARG_TYPE.throw(
        global,
        format_args!(
            "The \"destination\" argument must be of type path, file descriptor, or Blob. Received {received}"
        ),
    ))
}

/// `BunWriteOptions`: `createPath` (an empty string is no answer) and `mode`, from 0 to 0o777.
fn write_options(global: &JSGlobalObject, options: &JSValue) -> JsResult<WriteFileOptions> {
    if options.is_undefined_or_null() {
        return Ok(WriteFileOptions::default());
    }
    if !options.is_object() {
        let received = bun_js::bindgen::determine_specific_type(global, options)?;
        return Err(ErrorCode::INVALID_ARG_TYPE.throw(
            global,
            format_args!("The \"options\" argument must be of type object. Received {received}"),
        ));
    }
    let mkdirp_if_not_exists = match options.get(global, "createPath")? {
        Some(value) if value.is_null() || value.as_string().is_some_and(|s| s.is_empty()) => None,
        Some(value) => Some(value.to_boolean()),
        None => None,
    };
    let mode = match options.get(global, "mode")? {
        Some(value) if value.is_null() => None,
        Some(value) => {
            let Some(mode) = value.get_number() else {
                let received = bun_js::bindgen::determine_specific_type(global, &value)?;
                return Err(ErrorCode::INVALID_ARG_TYPE.throw(
                    global,
                    format_args!("The \"mode\" property must be of type number. Received {received}"),
                ));
            };
            let mode = mode as i64;
            if !(0..=0o777).contains(&mode) {
                let range = bun_core::fmt::OutOfRangeOptions {
                    field_name: b"mode",
                    min: 0,
                    max: 0o777,
                    msg: b"",
                };
                return Err(ErrorCode::OUT_OF_RANGE.throw(
                    global,
                    format_args!("{}", bun_core::fmt::out_of_range(mode, range)),
                ));
            }
            Some(mode as Mode)
        }
        None => None,
    };
    Ok(WriteFileOptions {
        mkdirp_if_not_exists,
        mode,
    })
}

/// `write_file_internal`: a short string or buffer is written at once; anything else is made a Blob, which is
/// written or copied to the destination.
pub(crate) fn write_file_internal(
    global: &JSGlobalObject,
    destination: Destination,
    data: &JSValue,
    options: WriteFileOptions,
) -> JsResult<JSValue> {
    if data.is_undefined_or_null() {
        return Err(ErrorCode::INVALID_ARG_TYPE.throw(
            global,
            format_args!("Bun.write(pathOrFdOrBlob, blob) expects a Blob-y thing to write"),
        ));
    }
    if let Destination::Blob(blob) = &destination
        && let Some(file) = blob.file()
    {
        file.last_modified.set(store::INIT_TIMESTAMP);
    }
    let pathlike = match &destination {
        Destination::Path(path) => path.clone(),
        Destination::Blob(blob) => blob
            .file()
            .expect("validate_writable_blob: a file")
            .pathlike
            .clone(),
    };
    if pathlike.path().is_some_and(|path| path.starts_with(b"s3://")) {
        return Err(global.throw(format_args!("S3 (an s3:// path) is not implemented on Boa yet")));
    }
    if options.mkdirp_if_not_exists == Some(true)
        && matches!(destination, Destination::Blob(_))
        && matches!(pathlike, PathOrFileDescriptor::Fd(_))
    {
        return Err(ErrorCode::INVALID_ARG_TYPE.throw(
            global,
            format_args!("Cannot create a directory for a file descriptor"),
        ));
    }

    let fast_path_ok = match &destination {
        Destination::Path(_) => true,
        Destination::Blob(blob) => {
            blob.offset == 0
                && !blob.file().is_some_and(|file| {
                    file.mode.get() != 0 && bun_sys::S::ISREG(file.mode.get())
                })
        }
    };
    if fast_path_ok {
        let bytes = if data.is_string() {
            if data.get_length(global)? < FAST_PATH_LIMIT {
                Some(data.to_utf8(global)?.into_bytes())
            } else {
                None
            }
        } else {
            engine::buffer_source_bytes(global, data)
                .filter(|bytes| (bytes.len() as u64) < FAST_PATH_LIMIT)
        };
        if let Some(bytes) = bytes
            && let Some(result) = write_fast(global, &pathlike, &bytes)
        {
            return result;
        }
    }

    let destination = match destination {
        Destination::Path(path) => Blob::find_or_create_file_from_path(path),
        Destination::Blob(blob) => blob,
    };
    let source = Blob::get(global, data, false)?;
    write_file_with_source_destination(global, &source, &destination, &options)
}

/// Writes all of `bytes`, waiting where a non-blocking descriptor is full.
pub(crate) fn write_all(fd: Fd, mut bytes: &[u8]) -> bun_sys::Result<usize> {
    let mut written = 0;
    while !bytes.is_empty() {
        match bun_sys::write(fd, bytes) {
            Ok(0) => break,
            Ok(wrote) => {
                written += wrote;
                bytes = &bytes[wrote..];
            }
            Err(err) if err.get_errno() == E::EAGAIN => store::wait_for(fd, libc::POLLOUT),
            Err(err) => return Err(err),
        }
    }
    Ok(written)
}

/// `write_string_to_file_fast` / `write_bytes_to_file_fast`: a path is opened without `O_TRUNC` and truncated to
/// what was written; a descriptor is written to as it is, and truncated only for nothing. `None` where the file's
/// directory does not exist: the slow path makes it.
fn write_fast(
    global: &JSGlobalObject,
    pathlike: &PathOrFileDescriptor,
    bytes: &[u8],
) -> Option<JsResult<JSValue>> {
    let result = match pathlike {
        PathOrFileDescriptor::Path(path) => {
            let flags = O::WRONLY | O::CREAT | O::NONBLOCK | O::CLOEXEC;
            let file = match bun_sys::File::open(&Path::from(path.as_slice()), flags, WRITE_PERMISSIONS) {
                Ok(file) => file,
                Err(err) if err.get_errno() == E::ENOENT => return None,
                Err(err) => return Some(reject(global, err.with_path(path))),
            };
            match write_all(file.fd(), bytes) {
                Ok(written) => {
                    let _ = bun_sys::ftruncate(file.fd(), written as i64);
                    Ok(written)
                }
                Err(err) => Err(err.with_path(path)),
            }
        }
        PathOrFileDescriptor::Fd(fd) => {
            let written = write_all(*fd, bytes);
            if bytes.is_empty() {
                let _ = bun_sys::ftruncate(*fd, 0);
            }
            written
        }
    };
    Some(match result {
        Ok(written) => engine::resolved(global, JSValue::js_number(written as f64)),
        Err(err) => reject(global, err),
    })
}

fn write_file_with_source_destination(
    global: &JSGlobalObject,
    source: &Blob,
    destination: &Blob,
    options: &WriteFileOptions,
) -> JsResult<JSValue> {
    let destination_file = destination
        .file()
        .expect("validate_writable_blob: a file");
    let Some(source_store) = &source.store else {
        return write_empty(global, destination_file, options);
    };
    match &source_store.data {
        Data::Bytes(_) => write_bytes(
            global,
            destination_file,
            source.shared_view(),
            options.mkdirp_if_not_exists.unwrap_or(true),
        ),
        Data::File(source_file) => copy_file(global, source_file, destination, options),
    }
}

/// Opens the file to write it from its start, making its directory first if it is missing and `mkdirp`.
fn open_destination(
    file: &store::File,
    mut mkdirp: bool,
    mode: Mode,
) -> Result<Opened, SystemError> {
    let path = match &file.pathlike {
        PathOrFileDescriptor::Fd(fd) => return Ok(Opened::Borrowed(*fd)),
        PathOrFileDescriptor::Path(path) => path,
    };
    let path_z = Path::from(path.as_slice());
    let flags = O::WRONLY | O::CREAT | O::TRUNC | O::NONBLOCK | O::CLOEXEC;
    creating_parents(&mut mkdirp, path, || bun_sys::File::open(&path_z, flags, mode))
        .map(Opened::Owned)
        .map_err(|err| SystemError::from(err.with_path(path)))
}

/// `WriteFile`: all of the source's bytes, to a file opened with `O_TRUNC` or to a descriptor.
fn write_bytes(
    global: &JSGlobalObject,
    destination: &store::File,
    bytes: &[u8],
    mkdirp: bool,
) -> JsResult<JSValue> {
    let opened = match open_destination(destination, mkdirp, DEFAULT_PERMISSION) {
        Ok(opened) => opened,
        Err(error) => return reject(global, error),
    };
    match write_all(opened.fd(), bytes) {
        Ok(written) => engine::resolved(global, JSValue::js_number(written as f64)),
        Err(err) => reject(global, err.with_path(destination.pathlike.path().unwrap_or_default())),
    }
}

/// `write_file_with_empty_source_to_destination`: nothing to write truncates the file, which is made if missing.
fn write_empty(
    global: &JSGlobalObject,
    file: &store::File,
    options: &WriteFileOptions,
) -> JsResult<JSValue> {
    let path = match &file.pathlike {
        PathOrFileDescriptor::Fd(fd) => {
            return match bun_sys::ftruncate(*fd, 0) {
                Ok(()) => engine::resolved(global, JSValue::js_number(0.0)),
                Err(err) => reject(global, err),
            };
        }
        PathOrFileDescriptor::Path(path) => path,
    };
    let path_z = Path::from(path.as_slice());
    let Err(mut err) = bun_sys::truncate(&path_z, 0) else {
        return engine::resolved(global, JSValue::js_number(0.0));
    };
    // truncate() may say EPERM where the directory does not exist (#6336).
    if err.get_errno() == E::EPERM {
        err = bun_sys::Error::from_code(E::ENOENT, err.syscall);
    }
    if err.get_errno() == E::ENOENT && options.mkdirp_if_not_exists != Some(false) {
        let created = mkdirp_parent(path).and_then(|()| {
            bun_sys::File::open(
                &path_z,
                O::CREAT | O::TRUNC,
                options.mode.unwrap_or(DEFAULT_PERMISSION),
            )
        });
        match created {
            Ok(_) => return engine::resolved(global, JSValue::js_number(0.0)),
            Err(error) => err = error,
        }
    }
    reject(global, err.with_path(path))
}

/// "That doesn't work on folders", as copy_file.rs says of a directory to copy.
fn unsupported_directory_error() -> SystemError {
    SystemError {
        errno: bun_sys::SystemErrno::EISDIR as i32,
        message: "That doesn't work on folders".to_owned(),
        syscall: Some("fstat"),
        ..SystemError::default()
    }
}

/// `CopyFile`: the source file, from the destination Blob's offset and up to its size, into the destination.
fn copy_file(
    global: &JSGlobalObject,
    source: &store::File,
    destination: &Blob,
    options: &WriteFileOptions,
) -> JsResult<JSValue> {
    match copy(source, destination, options) {
        Ok(copied) => engine::resolved(global, JSValue::js_number(copied as f64)),
        Err(error) => reject(global, error),
    }
}

fn copy(
    source: &store::File,
    destination: &Blob,
    options: &WriteFileOptions,
) -> Result<u64, SystemError> {
    let destination_file = destination
        .file()
        .expect("validate_writable_blob: a file");
    let source_path = source.pathlike.path().unwrap_or_default();
    let destination_path = destination_file.pathlike.path().unwrap_or_default();
    // Source first: if it fails, no destination file is left behind.
    let from = source.open_for_read()?;
    let to = open_destination(
        destination_file,
        options.mkdirp_if_not_exists.unwrap_or(true),
        options.mode.unwrap_or(DEFAULT_PERMISSION),
    )?;
    // open() sets the mode of a new file only.
    let set_mode = || match options.mode {
        Some(mode) => bun_sys::fchmod(to.fd(), mode)
            .map_err(|err| SystemError::from(err.with_path(destination_path))),
        None => Ok(()),
    };
    let stat = match bun_sys::fstat(from.fd()) {
        Ok(stat) => stat,
        Err(err) => {
            let _ = set_mode();
            return Err(SystemError::from(err.with_path(source_path)));
        }
    };
    let mode = stat.st_mode as Mode;
    if bun_sys::S::ISDIR(mode) {
        return Err(unsupported_directory_error());
    }
    let offset = destination.offset;
    let mut max_length = destination.size;
    // Only a regular file's st_size is a length.
    if stat.st_size > 0 && bun_sys::S::ISREG(mode) {
        max_length = (stat.st_size as u64).min(max_length).saturating_sub(offset);
    }
    if offset > 0 {
        let _ = bun_sys::set_file_offset(from.fd(), offset);
    }
    let mut copied = 0u64;
    let mut chunk = vec![0u8; 64 * 1024];
    while copied < max_length {
        let len = (chunk.len() as u64).min(max_length - copied) as usize;
        let read = match bun_sys::read(from.fd(), &mut chunk[..len]) {
            Ok(0) => break,
            Ok(read) => read,
            Err(err) if err.get_errno() == E::EAGAIN => {
                store::wait_for(from.fd(), libc::POLLIN);
                continue;
            }
            Err(err) => return Err(SystemError::from(err.with_path(source_path))),
        };
        write_all(to.fd(), &chunk[..read])
            .map_err(|err| SystemError::from(err.with_path(destination_path)))?;
        copied += read as u64;
    }
    set_mode()?;
    Ok(copied)
}
