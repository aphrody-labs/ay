//! `Bun.spawn` and `Bun.spawnSync`, declared in src/runtime/api/bun/js_bun_spawn_bindings.bind.ts:
//! src/runtime/api/bun/js_bun_spawn_bindings.rs on `bun_js`, over the same `bun_spawn_sys::spawn_process_posix`
//! (and so `posix_spawn_bun`, in Rust in `bun_spawn_rs`).
//!
//! The options are read in the order the JavaScriptCore build reads them, with its errors. What differs:
//! - spawnSync waits with `poll(2)` on the pipes and the pidfd instead of an isolated event loop: there is no
//!   JavaScript to run while it waits on Boa either.
//! - Not on Boa yet: `ipc` and `serialization`, `terminal`, `signal` (an `AbortSignal`), `cgroup`, a
//!   `ReadableStream` (or a `Response` or `Request` whose body is still arriving) as stdin, and extra stdio
//!   (`stdio[3]` and on) other than a descriptor, `"inherit"` and `"ignore"`. Each throws and names itself.

use core::ffi::c_int;

use bun_core::{CStrRef, CStringList, SignalCode};
use bun_js::bindgen::{JsLocal, JsScope};
use bun_js::{ErrorCode, JSGlobalObject, JSObject, JSValue, JsResult};
use bun_spawn_sys::{PosixSpawnOptions, PosixSpawnResult, PosixStdio, Rusage};
use bun_sys::Fd;

use crate::api::bun::resource_usage::ResourceUsage;
use crate::sys_js::SystemError;
use crate::webcore::blob::Blob;
use crate::webcore::blob::store::{Data as StoreData, PathOrFileDescriptor};

/// What `_PATH_DEFPATH` is with glibc: where a command is looked for when `PATH` is empty.
const DEFAULT_PATH_FOR_SPAWN: &[u8] = b"/usr/bin:/bin";

/// `Stdio` of src/runtime/api/bun/spawn/stdio.rs, for what Boa has.
pub(crate) enum Stdio {
    Inherit,
    Ignore,
    Pipe,
    Fd(Fd),
    Path(Vec<u8>),
    /// The bytes stdin is given.
    Bytes(Vec<u8>),
}

impl Stdio {
    pub(crate) fn is_piped(&self) -> bool {
        matches!(self, Stdio::Pipe | Stdio::Bytes(_))
    }

    fn as_spawn_option(&self) -> PosixStdio {
        match self {
            Stdio::Inherit => PosixStdio::Inherit,
            Stdio::Ignore => PosixStdio::Ignore,
            Stdio::Pipe | Stdio::Bytes(_) => PosixStdio::Buffer,
            Stdio::Fd(fd) => PosixStdio::Pipe(*fd),
            Stdio::Path(path) => PosixStdio::Path(path.clone().into_boxed_slice()),
        }
    }
}

/// Everything a spawn is told, read from its arguments.
pub(crate) struct SpawnArgs {
    /// The file to run, NUL-terminated.
    pub(crate) file: Vec<u8>,
    /// `argv`, the first of which is `argv0` where it was given.
    pub(crate) argv: Vec<Vec<u8>>,
    /// Empty: this process's.
    pub(crate) cwd: Vec<u8>,
    /// `KEY=value`.
    pub(crate) env: Vec<Vec<u8>>,
    pub(crate) stdio: [Stdio; 3],
    pub(crate) extra: Vec<Stdio>,
    pub(crate) detached: bool,
    pub(crate) uid: Option<u32>,
    pub(crate) gid: Option<u32>,
    /// Milliseconds.
    pub(crate) timeout: Option<u32>,
    pub(crate) kill_signal: c_int,
    pub(crate) max_buffer: Option<i64>,
    pub(crate) on_exit: Option<JSValue>,
    pub(crate) lazy: bool,
}

fn invalid(global: &JSGlobalObject, args: core::fmt::Arguments<'_>) -> bun_js::JsError {
    ErrorCode::INVALID_ARG_TYPE.throw(global, args)
}

fn not_on_boa(global: &JSGlobalObject, what: &str) -> bun_js::JsError {
    global.throw(format_args!("{what} is not implemented on Boa yet"))
}

/// `value[key]` where it is truthy, as `JSValue::get_truthy`.
fn get_truthy(global: &JSGlobalObject, object: &JSValue, key: &str) -> JsResult<Option<JSValue>> {
    Ok(object.get(global, key)?.filter(JSValue::to_boolean))
}

fn quoted(bytes: &[u8]) -> String {
    format!("{}", bun_core::fmt::quote(bytes))
}

fn has_nul(bytes: &[u8]) -> bool {
    bun_core::strings::contains_char(bytes, 0)
}

/// What a child inherits without `env`: the environment the process started with, as Bun's env loader has it
/// (an assignment to `process.env` does not reach it, in the JavaScriptCore build either).
fn process_env() -> Vec<(String, String)> {
    std::env::vars_os()
        .map(|(key, value)| (key.to_string_lossy().into_owned(), value.to_string_lossy().into_owned()))
        .collect()
}

/// A variable of [`process_env`].
pub(crate) fn env_var(name: &str) -> Option<String> {
    std::env::var_os(name).map(|value| value.to_string_lossy().into_owned())
}

fn env_entries(global: &JSGlobalObject, env: &JSObject) -> JsResult<Vec<(String, String)>> {
    let mut entries = Vec::new();
    for key in env.keys(global)? {
        let value = env.get(global, &key)?;
        if value.is_undefined() {
            continue;
        }
        entries.push((key, value.to_utf8(global)?));
    }
    Ok(entries)
}

/// `append_envp_from_js`: the lines of `env`, each checked for a NUL.
fn envp_from_js(global: &JSGlobalObject, env: &JSObject) -> JsResult<Vec<Vec<u8>>> {
    let mut lines = Vec::new();
    for (key, value) in env_entries(global, env)? {
        if has_nul(key.as_bytes()) || has_nul(value.as_bytes()) {
            // The key or the value as it is, NUL included.
            let received = if has_nul(key.as_bytes()) { &key } else { &value };
            return Err(ErrorCode::INVALID_ARG_VALUE.throw(
                global,
                format_args!(
                    "The property 'options.env['{key}']' must be a string without null bytes. Received \"{received}\""
                ),
            ));
        }
        lines.push(format!("{key}={value}").into_bytes());
    }
    Ok(lines)
}

fn path_of(lines: &[Vec<u8>]) -> Vec<u8> {
    lines
        .iter()
        .rev()
        .find_map(|line| line.strip_prefix(b"PATH="))
        .map(<[u8]>::to_vec)
        .unwrap_or_default()
}

/// The error of a command that is nowhere in `PATH`: `Executable not found in $PATH: "x"`.
fn command_not_found(global: &JSGlobalObject, command: &[u8]) -> bun_js::JsError {
    let error = SystemError {
        errno: -libc::ENOENT,
        code: Some("ENOENT"),
        message: format!(
            "Executable not found in $PATH: \"{}\"",
            String::from_utf8_lossy(command)
        ),
        path: Some(command.to_vec()),
        ..Default::default()
    };
    global.throw_value(error.to_error_instance(global))
}

/// `get_argv0` and `get_argv`: the file to run, found in `path` as execvpe would, and the strings of `argv`.
fn get_argv(
    global: &JSGlobalObject,
    cmd: &JSValue,
    path: &[u8],
    cwd: &[u8],
    argv0: Option<Vec<u8>>,
) -> JsResult<(Vec<u8>, Vec<Vec<u8>>)> {
    if cmd.is_undefined_or_null() {
        return Err(invalid(global, format_args!("cmd must be an array of strings")));
    }
    let length = cmd.get_length(global)?;
    if length == 0 {
        return Err(invalid(global, format_args!("cmd must not be empty")));
    }
    if length > u64::from(u32::MAX - 2) {
        return Err(invalid(global, format_args!("cmd array is too large")));
    }
    let element = |index: u64| -> JsResult<JSValue> {
        match cmd.as_string() {
            Some(text) => Ok(text
                .chars()
                .nth(index as usize)
                .map_or(JSValue::UNDEFINED, |c| JSValue::from_utf8(c.to_string().as_bytes()))),
            None => cmd.get_index(global, index as u32),
        }
    };
    let arg0 = element(0)?.to_utf8(global)?.into_bytes();
    if has_nul(&arg0) {
        return Err(ErrorCode::INVALID_ARG_VALUE.throw(
            global,
            format_args!(
                "The argument 'args[0]' must be a string without null bytes. Received {}",
                quoted(&arg0)
            ),
        ));
    }
    let names_a_file = bun_core::strings::contains_char(&arg0, b'/');
    let path = if names_a_file {
        &b""[..]
    } else if !path.is_empty() {
        path
    } else {
        DEFAULT_PATH_FOR_SPAWN
    };
    let mut file = if path.is_empty() {
        arg0.clone()
    } else {
        match bun_which::which_for_spawn(path, cwd, &arg0) {
            Some(resolved) => resolved.as_bytes().to_vec(),
            None => return Err(command_not_found(global, &arg0)),
        }
    };
    file.push(0);
    let mut argv = Vec::with_capacity(length as usize);
    argv.push(argv0.unwrap_or(arg0));
    for index in 1..length {
        let arg = element(index)?.to_utf8(global)?.into_bytes();
        if has_nul(&arg) {
            return Err(ErrorCode::INVALID_ARG_VALUE.throw(
                global,
                format_args!(
                    "The argument 'args[{index}]' must be a string without null bytes. Received \"{}\"",
                    String::from_utf8_lossy(&arg)
                ),
            ));
        }
        argv.push(arg);
    }
    Ok((file, argv))
}

/// `Stdio::extract`: what one of `stdin`, `stdout`, `stderr` or `stdio[i]` says.
fn extract_stdio(global: &JSGlobalObject, out: &mut Stdio, i: usize, value: &JSValue, is_sync: bool) -> JsResult<()> {
    if value.is_undefined() {
        return Ok(());
    }
    if value.is_null() {
        *out = Stdio::Ignore;
        return Ok(());
    }
    if let Some(text) = value.as_string() {
        *out = match text.as_str() {
            "inherit" => Stdio::Inherit,
            "ignore" => Stdio::Ignore,
            "pipe" | "overlapped" => Stdio::Pipe,
            "socket-fd" if i < 3 => {
                return Err(invalid(global, format_args!("stdio: 'socket-fd' is only supported at indices >= 3")));
            }
            "socket-fd" if is_sync => {
                return Err(invalid(global, format_args!("stdio: 'socket-fd' cannot be used with spawnSync")));
            }
            "socket-fd" => return Err(not_on_boa(global, "stdio 'socket-fd'")),
            // As JavaScriptCore's on POSIX, where an "ipc" stdio is a pipe that spawnSync never reads.
            "ipc" if is_sync => Stdio::Pipe,
            "ipc" => return Err(not_on_boa(global, "IPC")),
            _ => {
                return Err(invalid(
                    global,
                    format_args!(
                        "stdio must be an array of 'inherit', 'pipe', 'ignore', Bun.file(pathOrFd), number, or null"
                    ),
                ));
            }
        };
        return Ok(());
    }
    if value.is_number() {
        let fd = value.coerce_to_i32(global)?;
        if fd < 0 {
            return Err(invalid(global, format_args!("file descriptor must be a positive integer")));
        }
        *out = match fd {
            0 if i == 1 || i == 2 => {
                return Err(invalid(global, format_args!("stdin cannot be used for stdout or stderr")));
            }
            0 => Stdio::Inherit,
            1 | 2 if i == 0 => {
                return Err(invalid(global, format_args!("stdout and stderr cannot be used for stdin")));
            }
            1 if i == 1 => Stdio::Inherit,
            2 if i == 2 => Stdio::Inherit,
            fd => Stdio::Fd(Fd::from_uv(fd)),
        };
        return Ok(());
    }
    if let Some(blob) = value.as_class_ref::<Blob>() {
        let blob = blob.borrow(global)?;
        return extract_blob(global, out, i, &blob);
    }
    if let Some(response) = value.as_class_ref::<crate::webcore::response::Response>() {
        return extract_body(global, out, i, crate::webcore::response::take_body(&response, global)?.0, is_sync);
    }
    if let Some(request) = value.as_class_ref::<crate::webcore::request::Request>() {
        return extract_body(global, out, i, crate::webcore::response::take_body(&request, global)?.0, is_sync);
    }
    if let Some(bytes) = crate::webcore::body::buffer_source_bytes(value.as_engine(), crate::cx(global)) {
        // An empty ArrayBuffer is "ignore" (since Bun v1.0.34).
        if bytes.is_empty() {
            *out = Stdio::Ignore;
            return Ok(());
        }
        if i == 1 || i == 2 {
            return Err(invalid(global, format_args!("ArrayBufferView cannot be used for stdout/stderr yet")));
        }
        return set_stdin_bytes(global, out, i, bytes);
    }
    Err(invalid(global, format_args!("stdio must be an array of 'inherit', 'ignore', or null")))
}

/// `Stdio::extract_blob`: a file is its path or descriptor; bytes in memory are what stdin is given.
fn extract_blob(global: &JSGlobalObject, out: &mut Stdio, i: usize, blob: &Blob) -> JsResult<()> {
    if let Some(store) = blob.store.as_deref()
        && let StoreData::File(file) = &store.data
    {
        match &file.pathlike {
            PathOrFileDescriptor::Fd(fd) => {
                if usize::try_from(fd.uv()).ok() == Some(i) {
                    *out = Stdio::Inherit;
                } else {
                    *out = Stdio::Fd(*fd);
                }
            }
            PathOrFileDescriptor::Path(path) => *out = Stdio::Path(path.clone()),
        }
        return Ok(());
    }
    if i == 1 || i == 2 {
        return Err(invalid(global, format_args!("Blobs are immutable, and cannot be used for stdout/stderr")));
    }
    let bytes = blob.shared_view().to_vec();
    if bytes.is_empty() {
        *out = Stdio::Ignore;
        return Ok(());
    }
    set_stdin_bytes(global, out, i, bytes)
}

fn set_stdin_bytes(global: &JSGlobalObject, out: &mut Stdio, i: usize, bytes: Vec<u8>) -> JsResult<()> {
    if i != 0 {
        return Err(invalid(global, format_args!("Blob cannot be used for stdio[{i}] yet")));
    }
    *out = Stdio::Bytes(bytes);
    Ok(())
}

/// `Stdio::extract_body_value`: a body that is all there is its bytes; one still arriving is a stream.
fn extract_body(
    global: &JSGlobalObject,
    out: &mut Stdio,
    i: usize,
    body: crate::webcore::body::Body,
    is_sync: bool,
) -> JsResult<()> {
    match body.bytes() {
        Some(bytes) if bytes.is_empty() => {
            *out = Stdio::Ignore;
            Ok(())
        }
        Some(bytes) => {
            if i == 1 || i == 2 {
                return Err(invalid(global, format_args!("Blobs are immutable, and cannot be used for stdout/stderr")));
            }
            set_stdin_bytes(global, out, i, bytes.to_vec())
        }
        None if is_sync => Err(invalid(global, format_args!("ReadableStream cannot be used in sync mode"))),
        None => Err(not_on_boa(global, "A ReadableStream as stdin")),
    }
}

/// `validate_integer_range` of an option: an integer of `min..=max`, with Node's errors.
fn integer_option(global: &JSGlobalObject, value: &JSValue, name: &str, min: f64, max: f64) -> JsResult<f64> {
    let Some(number) = value.get_number() else {
        return Err(ErrorCode::INVALID_ARG_TYPE.throw(
            global,
            format_args!(
                "The \"{name}\" property must be of type number. Received {}",
                value.js_type_string(global)
            ),
        ));
    };
    if number.trunc() != number {
        return Err(ErrorCode::INVALID_ARG_TYPE.throw(
            global,
            format_args!("The \"{name}\" property must be of type integer. Received number"),
        ));
    }
    if number < min || number > max {
        let received = value.to_utf8(global)?;
        return Err(ErrorCode::OUT_OF_RANGE.throw(
            global,
            format_args!(
                "The value of \"{name}\" is out of range. It must be >= {} and <= {}. Received {received}",
                JSValue::js_number(min).to_utf8(global)?,
                JSValue::js_number(max).to_utf8(global)?
            ),
        ));
    }
    Ok(number)
}

/// `SignalCode.fromJS`: a number of 0..31 or a signal's name; `SIGTERM` for NaN, `""`, `undefined` and `null`.
pub(crate) fn signal_from_js(global: &JSGlobalObject, value: &JSValue) -> JsResult<c_int> {
    if let Some(number) = value.get_number() {
        if number.is_nan() {
            return Ok(SignalCode::DEFAULT as c_int);
        }
        if number.is_infinite() || number.trunc() != number {
            return Err(invalid(global, format_args!("Unknown signal")));
        }
        if number < 0.0 {
            return Err(invalid(global, format_args!("Invalid signal: must be >= 0")));
        }
        if number > 31.0 {
            return Err(invalid(global, format_args!("Invalid signal: must be < 32")));
        }
        return Ok(number as c_int);
    }
    if let Some(name) = value.as_string() {
        if name.is_empty() {
            return Ok(SignalCode::DEFAULT as c_int);
        }
        if let Some(code) = SignalCode::ALL.iter().find(|code| code.name() == name) {
            return Ok(*code as c_int);
        }
        let names: Vec<String> = SignalCode::ALL.iter().map(|code| format!("'{}'", code.name())).collect();
        let (last, rest) = names.split_last().expect("there are signals");
        return Err(invalid(global, format_args!("signal must be one of {} or {last}", rest.join(", "))));
    }
    if !value.is_undefined_or_null() {
        return Err(invalid(global, format_args!("Invalid signal: must be a string or an integer")));
    }
    Ok(SignalCode::DEFAULT as c_int)
}

/// `spawn_maybe_sync`'s reading of its arguments.
pub(crate) fn parse(global: &JSGlobalObject, args: JSValue, secondary: JSValue, is_sync: bool) -> JsResult<SpawnArgs> {
    if args.is_undefined_or_null() {
        return Err(invalid(global, format_args!("cmd must be an array")));
    }
    let (cmd, options) = if args.is_array() {
        (args, secondary)
    } else if !args.is_object() {
        return Err(invalid(global, format_args!("cmd must be an array")));
    } else if let Some(cmd) = get_truthy(global, &args, "cmd")? {
        (cmd, args)
    } else {
        return Err(invalid(global, format_args!("cmd must be an array")));
    };

    let mut cwd: Vec<u8> = bun_paths::fs::FileSystem::instance().top_level_dir().to_vec();
    let mut user_cwd = false;
    let mut stdio = [Stdio::Ignore, Stdio::Pipe, Stdio::Inherit];
    if is_sync {
        stdio[2] = Stdio::Pipe;
    }
    let mut env: Option<Vec<Vec<u8>>> = None;
    let mut path: Vec<u8> = env_var("PATH").unwrap_or_default().into_bytes();
    let mut argv0 = None;
    let mut extra = Vec::new();
    let mut parsed = SpawnArgs {
        file: Vec::new(),
        argv: Vec::new(),
        cwd: Vec::new(),
        env: Vec::new(),
        stdio: [Stdio::Ignore, Stdio::Ignore, Stdio::Ignore],
        extra: Vec::new(),
        detached: false,
        uid: None,
        gid: None,
        timeout: None,
        kill_signal: SignalCode::DEFAULT as c_int,
        max_buffer: None,
        on_exit: None,
        lazy: false,
    };

    if !options.is_object() {
        let (file, argv) = get_argv(global, &cmd, &path, &cwd, argv0)?;
        parsed.file = file;
        parsed.argv = argv;
    } else {
        if let Some(value) = get_truthy(global, &options, "argv0")? {
            let value = value.to_utf8(global)?.into_bytes();
            if !value.is_empty() {
                if has_nul(&value) {
                    return Err(ErrorCode::INVALID_ARG_VALUE.throw(
                        global,
                        format_args!(
                            "The property 'options.argv0' must be a string without null bytes. Received {}",
                            quoted(&value)
                        ),
                    ));
                }
                argv0 = Some(value);
            }
        }
        if let Some(value) = get_truthy(global, &options, "cwd")? {
            let value = value.to_utf8(global)?.into_bytes();
            if !value.is_empty() {
                if has_nul(&value) {
                    return Err(ErrorCode::INVALID_ARG_VALUE.throw(
                        global,
                        format_args!(
                            "The property 'options.cwd' must be a string without null bytes. Received {}",
                            quoted(&value)
                        ),
                    ));
                }
                cwd = value;
                user_cwd = true;
            }
        }
        if is_sync && get_truthy(global, &options, "terminal")?.is_some() {
            return Err(invalid(
                global,
                format_args!("terminal option is only supported for Bun.spawn, not Bun.spawnSync"),
            ));
        }
        if !is_sync && get_truthy(global, &options, "ipc")?.is_some_and(|ipc| ipc.is_callable()) {
            return Err(not_on_boa(global, "IPC"));
        }
        if get_truthy(global, &options, "signal")?.is_some() {
            return Err(not_on_boa(global, "The signal option (an AbortSignal)"));
        }
        if let Some(value) = get_truthy(global, &options, "onDisconnect")?
            && !value.is_callable()
        {
            return Err(invalid(global, format_args!("onDisconnect must be a function or undefined")));
        }
        if let Some(value) = get_truthy(global, &options, "onExit")? {
            if !value.is_callable() {
                return Err(invalid(global, format_args!("onExit must be a function or undefined")));
            }
            parsed.on_exit = Some(value);
        }
        if let Some(value) = get_truthy(global, &options, "env")? {
            let Some(object) = value.get_object() else {
                return Err(invalid(global, format_args!("env must be an object")));
            };
            let lines = envp_from_js(global, &object)?;
            path = path_of(&lines);
            env = Some(lines);
        }

        let (file, argv) = get_argv(global, &cmd, &path, &cwd, argv0)?;
        parsed.file = file;
        parsed.argv = argv;

        if let Some(value) = options.get(global, "stdio")?
            && !value.is_undefined_or_null()
        {
            if !value.is_array() {
                return Err(invalid(global, format_args!("stdio must be an array")));
            }
            let length = value.get_length(global)?;
            for index in 0..length {
                let item = value.get_index(global, index as u32)?;
                if index < 3 {
                    extract_stdio(global, &mut stdio[index as usize], index as usize, &item, is_sync)?;
                    continue;
                }
                let mut slot = Stdio::Ignore;
                extract_stdio(global, &mut slot, index as usize, &item, is_sync)?;
                if slot.is_piped() || matches!(slot, Stdio::Path(_)) {
                    return Err(not_on_boa(global, &format!("A pipe or a file at stdio[{index}]")));
                }
                extra.push(slot);
            }
        } else {
            if let Some(value) = options.get(global, "stdin")? {
                extract_stdio(global, &mut stdio[0], 0, &value, is_sync)?;
            }
            if let Some(value) = options.get(global, "stderr")? {
                extract_stdio(global, &mut stdio[2], 2, &value, is_sync)?;
            }
            if let Some(value) = options.get(global, "stdout")? {
                extract_stdio(global, &mut stdio[1], 1, &value, is_sync)?;
            }
        }

        if !is_sync
            && let Some(value) = options.get(global, "lazy")?
            && value.is_boolean()
        {
            parsed.lazy = value.as_boolean();
        }
        if let Some(value) = options.get(global, "detached")?
            && value.is_boolean()
        {
            parsed.detached = value.as_boolean();
        }
        let id_range = (f64::from(i32::MIN), f64::from(i32::MAX));
        if let Some(value) = options.get(global, "uid")?
            && !value.is_null()
        {
            parsed.uid = Some(integer_option(global, &value, "uid", id_range.0, id_range.1)? as i32 as u32);
        }
        if let Some(value) = options.get(global, "gid")?
            && !value.is_null()
        {
            parsed.gid = Some(integer_option(global, &value, "gid", id_range.0, id_range.1)? as i32 as u32);
        }
        if let Some(value) = options.get(global, "cgroup")?
            && !value.is_undefined_or_null()
        {
            return Err(not_on_boa(global, "The cgroup option"));
        }
        if let Some(value) = options.get(global, "timeout")?
            && !value.is_null()
        {
            let number = value.get_number();
            if number.is_some_and(f64::is_nan) {
                return Err(ErrorCode::OUT_OF_RANGE.throw(
                    global,
                    format_args!("The value of \"timeout\" is out of range. It must be a non-negative integer. Received NaN"),
                ));
            }
            // +Infinity is no timeout.
            if !number.is_some_and(|n| n.is_infinite() && n > 0.0) {
                let timeout = integer_option(global, &value, "timeout", 0.0, 9_007_199_254_740_991.0)?;
                if timeout > 0.0 {
                    parsed.timeout = Some((timeout as u64 as u32) & 0x7FFF_FFFF);
                }
            }
        }
        if let Some(value) = options.get(global, "killSignal")? {
            parsed.kill_signal = signal_from_js(global, &value)?;
            if parsed.kill_signal == 0 {
                return Err(ErrorCode::UNKNOWN_SIGNAL.throw(global, format_args!("Unknown signal: 0")));
            }
        }
        if let Some(value) = options.get(global, "maxBuffer")?
            && let Some(number) = value.get_number()
            && number.is_finite()
        {
            let max = number as i64;
            if max > 0 && stdio.iter().any(Stdio::is_piped) {
                parsed.max_buffer = Some(max);
            }
        }
        if !is_sync && get_truthy(global, &options, "terminal")?.is_some() {
            return Err(not_on_boa(global, "The terminal option"));
        }
    }

    parsed.cwd = if user_cwd { cwd } else { Vec::new() };
    parsed.env = match env {
        Some(lines) => lines,
        None => process_env()
            .into_iter()
            .map(|(key, value)| format!("{key}={value}").into_bytes())
            .collect(),
    };
    // spawnSync's stdin "pipe" has nothing to write: it is "ignore".
    if is_sync && matches!(stdio[0], Stdio::Pipe) {
        stdio[0] = Stdio::Ignore;
    }
    parsed.stdio = stdio;
    parsed.extra = extra;
    Ok(parsed)
}

/// Spawns the child of `args`, or throws what JavaScriptCore throws for a spawn that failed.
pub(crate) fn start(global: &JSGlobalObject, args: &SpawnArgs) -> JsResult<PosixSpawnResult> {
    let argv = CStringList::from_parts(args.argv.iter().map(|arg| [&arg[..]]));
    let envp = CStringList::from_parts(args.env.iter().map(|line| [&line[..]]));
    let options = PosixSpawnOptions {
        stdin: args.stdio[0].as_spawn_option(),
        stdout: args.stdio[1].as_spawn_option(),
        stderr: args.stdio[2].as_spawn_option(),
        extra_fds: args.extra.iter().map(Stdio::as_spawn_option).collect(),
        cwd: args.cwd.clone().into_boxed_slice(),
        detached: args.detached,
        uid: args.uid,
        gid: args.gid,
        argv0: Some(CStrRef::from_slice_with_nul(&args.file)),
        ..Default::default()
    };
    let display_path = &args.argv[0][..];
    match bun_spawn_sys::spawn_process_posix(&options, argv.as_list(), Some(envp.as_list())) {
        Err(error) => Err(global.throw(format_args!("{}: failed to spawn process", error.name()))),
        Ok(Err(error)) => {
            let error = match error.get_errno() {
                bun_sys::E::EACCES
                | bun_sys::E::ENOENT
                | bun_sys::E::EPERM
                | bun_sys::E::EISDIR
                | bun_sys::E::ENOTDIR
                | bun_sys::E::EMFILE
                | bun_sys::E::ENFILE
                    if !display_path.is_empty() =>
                {
                    error.with_path(display_path)
                }
                _ => error,
            };
            Err(global.throw_value(SystemError::from(&error).to_error_instance(global)))
        }
        Ok(Ok(spawned)) => Ok(spawned),
    }
}

/// `Bun.spawn(cmd, options?)` or `Bun.spawn({ cmd, ...options })`.
pub(crate) fn spawn<'a>(cx: &mut JsScope<'a>, cmd_or_options: JsLocal<'a>, options: JsLocal<'a>) -> JsResult<JSValue> {
    let global = cx.heap.global_object();
    let args = parse(global, cmd_or_options.to_js(), options.to_js(), false)?;
    crate::api::bun::subprocess::spawn(global, args)
}

/// `Bun.spawnSync(cmd, options?)` or `Bun.spawnSync({ cmd, ...options })`.
pub(crate) fn spawn_sync<'a>(
    cx: &mut JsScope<'a>,
    cmd_or_options: JsLocal<'a>,
    options: JsLocal<'a>,
) -> JsResult<JSValue> {
    let global = cx.heap.global_object();
    let args = parse(global, cmd_or_options.to_js(), options.to_js(), true)?;
    let spawned = start(global, &args)?;
    let outcome = run_sync(spawned, &args);

    let result = JSValue::create_empty_object(global, 0);
    let (exit_code, signal_code) = exit_to_js(&outcome.status);
    result.put(global, "exitCode", exit_code);
    if !signal_code.is_undefined_or_null() {
        result.put(global, "signalCode", signal_code);
    }
    let output = |stdio: &Stdio, bytes: Vec<u8>| -> JsResult<JSValue> {
        if stdio.is_piped() { JSValue::create_buffer(global, &bytes) } else { Ok(JSValue::UNDEFINED) }
    };
    result.put(global, "stdout", output(&args.stdio[1], outcome.stdout)?);
    result.put(global, "stderr", output(&args.stdio[2], outcome.stderr)?);
    result.put(
        global,
        "success",
        JSValue::js_boolean(matches!(outcome.status, Status::Exited(0))),
    );
    result.put(global, "resourceUsage", bun_js::class::to_js(global, ResourceUsage::new(outcome.rusage))?);
    if args.timeout.is_some() {
        result.put(global, "exitedDueToTimeout", JSValue::js_boolean(outcome.timed_out));
    }
    if args.max_buffer.is_some() {
        result.put(global, "exitedDueToMaxBuffer", JSValue::js_boolean(outcome.max_buffer_exceeded));
    }
    result.put(global, "pid", JSValue::js_number_from_int32(outcome.pid));
    Ok(result)
}

/// How a child ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Status {
    Exited(c_int),
    Signaled(c_int),
}

impl Status {
    pub(crate) fn of_wait_status(status: c_int) -> Status {
        if libc::WIFSIGNALED(status) {
            Status::Signaled(libc::WTERMSIG(status))
        } else {
            Status::Exited(libc::WEXITSTATUS(status))
        }
    }
}

/// `exitCode` and `signalCode`: a number and `null`, or `null` and the signal's name (its number if it has none).
pub(crate) fn exit_to_js(status: &Status) -> (JSValue, JSValue) {
    match *status {
        Status::Exited(code) => (JSValue::js_number_from_int32(code), JSValue::NULL),
        Status::Signaled(signal) => (JSValue::NULL, signal_to_js(signal)),
    }
}

pub(crate) fn signal_to_js(signal: c_int) -> JSValue {
    match SignalCode::from_number(signal) {
        Some(code) => JSValue::from_utf8(code.name().as_bytes()),
        None => JSValue::js_number_from_int32(signal),
    }
}

struct SyncOutcome {
    pid: c_int,
    status: Status,
    rusage: Rusage,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    timed_out: bool,
    max_buffer_exceeded: bool,
}

/// What a nonblocking read of `fd` gives now: the bytes, and whether it reached the end (or failed).
pub(crate) fn read_available(fd: Fd, into: &mut Vec<u8>) -> bool {
    let mut chunk = [0u8; 64 * 1024];
    loop {
        // SAFETY: a read into a stack buffer of its length.
        let n = unsafe { libc::read(fd.native(), chunk.as_mut_ptr().cast(), chunk.len()) };
        if n > 0 {
            into.extend_from_slice(&chunk[..n as usize]);
            continue;
        }
        if n == 0 {
            return true;
        }
        match std::io::Error::last_os_error().raw_os_error() {
            Some(libc::EINTR) => continue,
            Some(libc::EAGAIN) => return false,
            _ => return true,
        }
    }
}

/// Writes what the pipe takes now; `true` once all of `bytes` is written or the pipe is gone.
pub(crate) fn write_available(fd: Fd, bytes: &[u8], written: &mut usize) -> bool {
    while *written < bytes.len() {
        let rest = &bytes[*written..];
        // SAFETY: a write of a slice of its length.
        let n = unsafe { libc::write(fd.native(), rest.as_ptr().cast(), rest.len()) };
        if n >= 0 {
            *written += n as usize;
            continue;
        }
        match std::io::Error::last_os_error().raw_os_error() {
            Some(libc::EINTR) => continue,
            Some(libc::EAGAIN) => return false,
            _ => return true,
        }
    }
    true
}

/// `wait4` of the child, once it ended (`WNOHANG` unless `block`).
pub(crate) fn reap(pid: c_int, block: bool, rusage: &mut Rusage) -> Option<Status> {
    let mut status: c_int = 0;
    loop {
        // SAFETY: wait4 writes the status and the usage it is given.
        let rc = unsafe { libc::wait4(pid, &mut status, if block { 0 } else { libc::WNOHANG }, rusage) };
        if rc == pid {
            return Some(Status::of_wait_status(status));
        }
        if rc == -1 && std::io::Error::last_os_error().raw_os_error() == Some(libc::EINTR) {
            continue;
        }
        // 0 (still running), or ECHILD: not ours to wait on any more.
        return if rc == -1 { Some(Status::Exited(0)) } else { None };
    }
}

/// The wait of spawnSync: the pipes are read and stdin written as they are ready, until the child ended and every
/// pipe reached its end, as the isolated event loop's turns do. A timeout or a `maxBuffer` that is exceeded kills
/// the child with `killSignal` and stops waiting on the pipes (a grandchild may hold them, as Node's
/// `SyncProcessRunner::Kill`).
fn run_sync(mut spawned: PosixSpawnResult, args: &SpawnArgs) -> SyncOutcome {
    let pid = spawned.pid;
    let mut stdin = spawned.stdin.take();
    let mut stdout = spawned.stdout.take();
    let mut stderr = spawned.stderr.take();
    let pidfd = spawned.pidfd.take();
    let stdin_bytes: &[u8] = match &args.stdio[0] {
        Stdio::Bytes(bytes) => bytes,
        _ => {
            stdin = None;
            &[]
        }
    };
    let mut written = 0;
    let mut outcome = SyncOutcome {
        pid,
        status: Status::Exited(0),
        rusage: bun_spawn_sys::rusage_zeroed(),
        stdout: Vec::new(),
        stderr: Vec::new(),
        timed_out: false,
        max_buffer_exceeded: false,
    };
    // Without a pidfd (the child was gone before one was asked for) only the end of the pipes says when to reap.
    let mut exited = None;
    let deadline = args.timeout.map(|ms| std::time::Instant::now() + std::time::Duration::from_millis(u64::from(ms)));
    let kill = |signal: c_int| {
        // SAFETY: kill(2) takes integers.
        unsafe { libc::kill(pid, signal) };
    };
    loop {
        let waiting_on_exit = exited.is_none() && pidfd.is_some();
        if stdin.is_none() && stdout.is_none() && stderr.is_none() && !waiting_on_exit {
            break;
        }
        let mut fds: Vec<libc::pollfd> = Vec::with_capacity(4);
        let mut slots = Vec::with_capacity(4);
        let mut watch = |fd: Fd, events: i16, slot: u8, fds: &mut Vec<libc::pollfd>| {
            fds.push(libc::pollfd { fd: fd.native(), events, revents: 0 });
            slots.push(slot);
        };
        if let Some(file) = &stdin {
            watch(file.handle(), libc::POLLOUT, 0, &mut fds);
        }
        if let Some(file) = &stdout {
            watch(file.handle(), libc::POLLIN, 1, &mut fds);
        }
        if let Some(file) = &stderr {
            watch(file.handle(), libc::POLLIN, 2, &mut fds);
        }
        if waiting_on_exit && let Some(file) = &pidfd {
            watch(file.handle(), libc::POLLIN, 3, &mut fds);
        }
        let timeout_ms = match deadline {
            Some(at) if !outcome.timed_out => {
                at.saturating_duration_since(std::time::Instant::now()).as_millis().min(c_int::MAX as u128) as c_int
            }
            _ => -1,
        };
        // SAFETY: `fds` is a live array of its length.
        let ready = unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, timeout_ms) };
        if ready == -1 {
            if std::io::Error::last_os_error().raw_os_error() == Some(libc::EINTR) {
                continue;
            }
            break;
        }
        if ready == 0 {
            outcome.timed_out = true;
            kill(args.kill_signal);
            stdin = None;
            stdout = None;
            stderr = None;
            continue;
        }
        for (poll, slot) in fds.iter().zip(&slots) {
            if poll.revents == 0 {
                continue;
            }
            match slot {
                0 => {
                    if write_available(Fd::from_native(poll.fd), stdin_bytes, &mut written) {
                        stdin = None;
                    }
                }
                1 | 2 => {
                    let (buffer, file) =
                        if *slot == 1 { (&mut outcome.stdout, &mut stdout) } else { (&mut outcome.stderr, &mut stderr) };
                    let done = read_available(Fd::from_native(poll.fd), buffer);
                    if done {
                        *file = None;
                    }
                    if let Some(max) = args.max_buffer
                        && buffer.len() as i64 > max
                        && !outcome.max_buffer_exceeded
                    {
                        outcome.max_buffer_exceeded = true;
                        kill(args.kill_signal);
                    }
                }
                _ => {
                    exited = reap(pid, false, &mut outcome.rusage);
                }
            }
        }
        if outcome.max_buffer_exceeded {
            stdin = None;
            stdout = None;
            stderr = None;
        }
    }
    outcome.status = match exited {
        Some(status) => status,
        None => reap(pid, true, &mut outcome.rusage).unwrap_or(Status::Exited(0)),
    };
    outcome
}
