//! The members of `process` that src/jsc/bindings/BunProcess.cpp defines in C++, on `bun_js`: `umask`, `hrtime`,
//! `hrtime.bigint`, `memoryUsage`, `memoryUsage.rss`, `cpuUsage`, `resourceUsage`, `report.getReport` and
//! `execPath` (node_process.rs's `get_exec_path`). js/bun.ts puts them on its `process`.

use bun_js::{CallFrame, ErrorCode, JSArray, JSFunction, JSGlobalObject, JSObject, JSValue, JsResult};

use crate::node::types::{invalid_arg_type, out_of_range, validate_uint32};
use crate::sys_js::SystemError;

const MAX_SAFE_INTEGER: f64 = 9_007_199_254_740_991.0;

fn object_of(global: &JSGlobalObject, members: &[(&str, JSValue)]) -> JSValue {
    let object = JSObject::create(global);
    for (name, value) in members {
        object.put(global, name, value.clone());
    }
    object.to_js()
}

fn number(n: impl Into<f64>) -> JSValue {
    JSValue::js_number(n.into())
}

/// helpers.h's `throwSystemError(scope, global, message, syscall, errno)`.
fn throw_system_error(global: &JSGlobalObject, message: &str, syscall: &'static str) -> bun_js::JsError {
    let errno = bun_sys::last_errno();
    let code = bun_sys::Error::from_code_int(errno, bun_sys::Tag::open)
        .uv_code_label()
        .map(|(code, _)| code);
    let error = SystemError {
        errno: -errno,
        code,
        message: message.to_owned(),
        syscall: Some(syscall),
        ..SystemError::default()
    };
    global.throw_value(error.to_error_instance(global))
}

/// A `getrusage(RUSAGE_SELF)`, or the system error C++ throws for it.
fn rusage(global: &JSGlobalObject, what: &str) -> JsResult<libc::rusage> {
    // SAFETY: an all-zero rusage is valid, and getrusage only writes it.
    let mut usage: libc::rusage = unsafe { core::mem::zeroed() };
    // SAFETY: as above.
    if unsafe { libc::getrusage(libc::RUSAGE_SELF, &mut usage) } != 0 {
        return Err(throw_system_error(global, what, "getrusage"));
    }
    Ok(usage)
}

fn micros(time: libc::timeval) -> f64 {
    1_000_000.0 * time.tv_sec as f64 + time.tv_usec as f64
}

/// `process.umask(mask?)`: the mask, and with `mask` the one it replaces.
pub fn umask(global: &JSGlobalObject, frame: &CallFrame<'_>) -> JsResult<JSValue> {
    let value = frame.argument(0);
    if frame.arguments_count() == 0 || value.is_undefined() {
        // SAFETY: umask cannot fail; the mask is put back.
        let current = unsafe { libc::umask(0) };
        // SAFETY: as above.
        unsafe { libc::umask(current) };
        return Ok(number(current as u32));
    }
    let mask = if value.is_string() {
        let text = value.to_utf8(global)?;
        match u32::from_str_radix(&text, 8) {
            Ok(mask) => mask,
            _ => {
                let shown = crate::node::types::inspect(global, &value)?;
                return Err(ErrorCode::INVALID_ARG_VALUE.throw(
                    global,
                    format_args!(
                        "The argument 'mask' must be a 32-bit unsigned integer or an octal string. Received {shown}"
                    ),
                ));
            }
        }
    } else {
        validate_uint32(global, &value, "mask", false)?
    };
    // SAFETY: as above.
    let previous = unsafe { libc::umask(mask as libc::mode_t) };
    Ok(number(previous as u32))
}

/// The nanoseconds since the process started, as `Bun__readHRTime` reads the VM's origin timer.
fn hrtime_ns() -> u64 {
    crate::web::performance::elapsed_ns()
}

/// `process.hrtime(time?)`: `[seconds, nanoseconds]`, less `time` where it is given.
pub fn hrtime(global: &JSGlobalObject, frame: &CallFrame<'_>) -> JsResult<JSValue> {
    let time = hrtime_ns();
    let mut seconds = (time / 1_000_000_000) as f64;
    let mut nanoseconds = (time % 1_000_000_000) as f64;
    let relative = frame.argument(0);
    if frame.arguments_count() > 0 && !relative.is_undefined() {
        if !relative.is_array() {
            return Err(invalid_arg_type(global, "time", "an instance of Array", &relative));
        }
        let length = relative.get_length(global)?;
        if length != 2 {
            return Err(out_of_range(global, "time", "2", &number(length as f64)));
        }
        let relative_seconds = relative.get_index(global, 0)?.to_number(global)?;
        let relative_nanoseconds = relative.get_index(global, 1)?.to_number(global)?;
        let borrow = nanoseconds < relative_nanoseconds;
        seconds = if borrow { seconds - relative_seconds - 1.0 } else { seconds - relative_seconds };
        nanoseconds = if borrow {
            nanoseconds + 1_000_000_000.0 - relative_nanoseconds
        } else {
            nanoseconds - relative_nanoseconds
        };
    }
    Ok(JSArray::create(global, &[number(seconds), number(nanoseconds)]).to_js())
}

/// `process.hrtime.bigint()`.
pub fn hrtime_bigint(_: &JSGlobalObject, _: &CallFrame<'_>) -> JsResult<JSValue> {
    Ok(JSValue::from_i128(i128::from(hrtime_ns())))
}

fn rss(global: &JSGlobalObject) -> JsResult<usize> {
    bun_sys::self_process_memory_usage()
        .ok_or_else(|| throw_system_error(global, "Failed to get memory usage", "memoryUsage"))
}

/// `process.memoryUsage()`: `{ rss, heapTotal, heapUsed, external, arrayBuffers }`. The heap is Boa's: what its
/// collector counts as allocated, and the threshold of its next collection.
pub fn memory_usage(global: &JSGlobalObject, _: &CallFrame<'_>) -> JsResult<JSValue> {
    let rss = rss(global)?;
    let (used, threshold) = boa_gc::heap_stats();
    Ok(object_of(
        global,
        &[
            ("rss", number(rss as f64)),
            ("heapTotal", number(used.max(threshold) as f64)),
            ("heapUsed", number(used as f64)),
            ("external", number(0)),
            ("arrayBuffers", number(0)),
        ],
    ))
}

/// `process.memoryUsage.rss()`.
pub fn memory_usage_rss(global: &JSGlobalObject, _: &CallFrame<'_>) -> JsResult<JSValue> {
    Ok(number(rss(global)? as f64))
}

/// `process.cpuUsage(prevValue?)`: `{ user, system }` in microseconds, less `prevValue`'s.
pub fn cpu_usage(global: &JSGlobalObject, frame: &CallFrame<'_>) -> JsResult<JSValue> {
    let usage = rusage(global, "Failed to get CPU usage")?;
    let mut user = micros(usage.ru_utime);
    let mut system = micros(usage.ru_stime);
    let previous = frame.argument(0);
    if frame.arguments_count() > 0 && !previous.is_undefined() {
        if !previous.is_object() {
            return Err(invalid_arg_type(global, "prevValue", "of type object", &previous));
        }
        let user_value = previous.get(global, "user")?.unwrap_or_default();
        let system_value = previous.get(global, "system")?.unwrap_or_default();
        for (name, value) in [("prevValue.user", &user_value), ("prevValue.system", &system_value)] {
            if !value.is_number() {
                return Err(invalid_arg_type(global, name, "of type number", value));
            }
        }
        for (name, value) in [("prevValue.user", &user_value), ("prevValue.system", &system_value)] {
            let n = value.as_number();
            if !(0.0..=MAX_SAFE_INTEGER).contains(&n) {
                let shown = crate::node::types::inspect(global, value)?;
                let error = ErrorCode::INVALID_ARG_VALUE
                    .fmt(global, format_args!("The property '{name}' is invalid. Received {shown}"));
                return Err(global.throw_value(range_error_of(global, error)?));
            }
        }
        user -= user_value.as_number();
        system -= system_value.as_number();
    }
    Ok(object_of(global, &[("user", number(user)), ("system", number(system))]))
}

/// `ERR_INVALID_ARG_VALUE` as a `RangeError`, as C++'s `INVALID_ARG_VALUE_RangeError`.
fn range_error_of(global: &JSGlobalObject, error: JSValue) -> JsResult<JSValue> {
    let message = error.get(global, "message")?.unwrap_or_default().to_utf8(global)?;
    let range = global.create_range_error_instance(format_args!("{message}"));
    range.put(global, "code", JSValue::from_utf8(b"ERR_INVALID_ARG_VALUE"));
    Ok(range)
}

/// `process.resourceUsage()`, in the order C++'s structure has the members.
pub fn resource_usage(global: &JSGlobalObject, _: &CallFrame<'_>) -> JsResult<JSValue> {
    let usage = rusage(global, "Failed to get resource usage")?;
    // Darwin counts ru_maxrss in bytes; Node reports kilobytes.
    let max_rss = if cfg!(target_os = "macos") { usage.ru_maxrss / 1024 } else { usage.ru_maxrss };
    Ok(object_of(
        global,
        &[
            ("userCPUTime", number(micros(usage.ru_utime))),
            ("systemCPUTime", number(micros(usage.ru_stime))),
            ("maxRSS", number(max_rss as f64)),
            ("sharedMemorySize", number(usage.ru_ixrss as f64)),
            ("unsharedDataSize", number(usage.ru_idrss as f64)),
            ("unsharedStackSize", number(usage.ru_isrss as f64)),
            ("minorPageFault", number(usage.ru_minflt as f64)),
            ("majorPageFault", number(usage.ru_majflt as f64)),
            ("swappedOut", number(usage.ru_nswap as f64)),
            ("fsRead", number(usage.ru_inblock as f64)),
            ("fsWrite", number(usage.ru_oublock as f64)),
            ("ipcSent", number(usage.ru_msgsnd as f64)),
            ("ipcReceived", number(usage.ru_msgrcv as f64)),
            ("signalsCount", number(usage.ru_nsignals as f64)),
            ("voluntaryContextSwitches", number(usage.ru_nvcsw as f64)),
            ("involuntaryContextSwitches", number(usage.ru_nivcsw as f64)),
        ],
    ))
}

/// `process.execPath`: the executable, or `argv[0]` where the system cannot say.
pub fn exec_path() -> Vec<u8> {
    match bun_core::self_exe_path() {
        Ok(path) => path.as_bytes().to_vec(),
        Err(_) => std::env::args_os()
            .next()
            .map(|arg| arg.into_encoded_bytes())
            .unwrap_or_default(),
    }
}

fn c_text(bytes: &[libc::c_char]) -> JSValue {
    let bytes: Vec<u8> = bytes.iter().take_while(|&&c| c != 0).map(|&c| c as u8).collect();
    JSValue::from_utf8(&bytes)
}

/// `process.report.getReport()`: `constructReportObjectComplete`, with what of `process` it reads (`execArgv`,
/// `arch`, `platform`, `versions`, `release`, `env`) read from `process` itself.
pub fn get_report(global: &JSGlobalObject, _: &CallFrame<'_>) -> JsResult<JSValue> {
    let process = global.to_js_value().get(global, "process")?.unwrap_or_default();
    let of_process = |name: &str| -> JsResult<JSValue> { Ok(process.get(global, name)?.unwrap_or_default()) };
    let empty_array = || JSArray::create(global, &[]).to_js();
    let usage = rusage(global, "Failed to get resource usage")?;

    let header = JSObject::create(global);
    let put = |object: &JSObject, name: &str, value: JSValue| object.put(global, name, value);
    put(&header, "reportVersion", number(3));
    put(&header, "event", JSValue::from_utf8(b"JavaScript API"));
    put(&header, "trigger", JSValue::from_utf8(b"GetReport"));
    put(&header, "filename", JSValue::NULL);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0.0, |since| since.as_secs_f64() * 1000.0);
    put(&header, "dumpEventTime", number(now).to_js_string(global)?);
    let date = global.evaluate_script("Date")?.construct(global, &[number(now)])?;
    let iso = date.get(global, "toISOString")?.unwrap_or_default().call(global, date, &[])?;
    put(&header, "dumpEventTimeStamp", iso);
    put(&header, "processId", number(std::process::id()));
    put(&header, "threadId", number(0));
    let cwd = std::env::current_dir().map_or_else(|_| ".".into(), |dir| dir.display().to_string());
    put(&header, "cwd", JSValue::from_utf8(cwd.as_bytes()));
    put(&header, "commandLine", of_process("execArgv")?);
    put(&header, "nodejsVersion", of_process("version")?);
    put(&header, "wordSize", number(64));
    put(&header, "arch", of_process("arch")?);
    put(&header, "platform", of_process("platform")?);
    put(&header, "componentVersions", of_process("versions")?);
    put(&header, "release", of_process("release")?);
    // SAFETY: an all-zero utsname is valid, and uname only writes it.
    let mut uname: libc::utsname = unsafe { core::mem::zeroed() };
    // SAFETY: as above.
    unsafe { libc::uname(&mut uname) };
    put(&header, "osName", c_text(&uname.sysname));
    put(&header, "osRelease", c_text(&uname.release));
    put(&header, "osVersion", c_text(&uname.version));
    put(&header, "osMachine", c_text(&uname.machine));
    let mut host = [0 as libc::c_char; 1024];
    // SAFETY: the buffer and its length.
    unsafe { libc::gethostname(host.as_mut_ptr(), host.len()) };
    put(&header, "host", c_text(&host));
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    {
        // SAFETY: glibc's static version string.
        let runtime = unsafe { core::ffi::CStr::from_ptr(libc::gnu_get_libc_version()) };
        let runtime = JSValue::from_utf8(runtime.to_bytes());
        put(&header, "glibcVersionCompiler", runtime.clone());
        put(&header, "glibcVersionRuntime", runtime);
    }
    put(&header, "cpus", empty_array());
    put(&header, "networkInterfaces", empty_array());

    let stack = JSObject::create(global);
    put(&stack, "message", JSValue::from_utf8(b"Error [ERR_SYNTHETIC]: JavaScript Callstack"));
    put(&stack, "stack", empty_array());
    put(&stack, "errorProperties", object_of(global, &[("code", JSValue::from_utf8(b"ERR_SYNTHETIC"))]));

    let spaces = JSObject::create(global);
    for space in [
        "read_only_space",
        "new_space",
        "old_space",
        "code_space",
        "shared_space",
        "new_large_object_space",
        "large_object_space",
        "code_large_object_space",
        "shared_large_object_space",
    ] {
        put(&spaces, space, JSObject::create(global).to_js());
    }
    let heap = JSObject::create(global);
    put(&heap, "totalMemory", number(bun_core::get_total_memory_size() as f64));
    for name in [
        "executableMemory",
        "totalCommittedMemory",
        "availableMemory",
        "totalGlobalHandlesMemory",
        "usedGlobalHandlesMemory",
        "usedMemory",
        "memoryLimit",
        "mallocedMemory",
        "externalMemory",
        "peakMallocedMemory",
    ] {
        put(&heap, name, number(0));
    }
    put(&heap, "nativeContextCount", number(1));
    put(&heap, "detachedContextCount", number(0));
    put(&heap, "doesZapGarbage", number(0));
    put(&heap, "heapSpaces", spaces.to_js());

    let rss = bun_sys::self_process_memory_usage().unwrap_or(0);
    let max_rss = bun_sys::self_process_peak_memory_usage().unwrap_or(0);
    let user = number(usage.ru_utime.tv_sec as f64);
    let resource = object_of(
        global,
        &[
            ("free_memory", number(usage.ru_maxrss as f64)),
            ("total_memory", number(usage.ru_maxrss as f64)),
            ("rss", number(rss as f64)),
            ("available_memory", number(usage.ru_maxrss as f64)),
            ("userCpuSeconds", user.clone()),
            ("kernelCpuSeconds", number(usage.ru_stime.tv_sec as f64)),
            ("cpuConsumptionPercent", user.clone()),
            ("userCpuConsumptionPercent", user.clone()),
            ("kernelCpuConsumptionPercent", user),
            ("maxRss", number(max_rss as f64)),
            (
                "pageFaults",
                object_of(
                    global,
                    &[
                        ("IORequired", number(usage.ru_majflt as f64)),
                        ("IONotRequired", number(usage.ru_minflt as f64)),
                    ],
                ),
            ),
            (
                "fsActivity",
                object_of(
                    global,
                    &[("reads", number(usage.ru_inblock as f64)), ("writes", number(usage.ru_oublock as f64))],
                ),
            ),
        ],
    );
    let zero_activity = || object_of(global, &[("reads", number(0)), ("writes", number(0))]);
    let thread = object_of(
        global,
        &[
            ("userCpuSeconds", number(0)),
            ("kernelCpuSeconds", number(0)),
            ("cpuConsumptionPercent", number(0)),
            ("userCpuConsumptionPercent", number(0)),
            ("kernelCpuConsumptionPercent", number(0)),
            ("fsActivity", zero_activity()),
        ],
    );

    let limits = JSObject::create(global);
    for (resource, label) in [
        (libc::RLIMIT_CORE, "core_file_size_blocks"),
        (libc::RLIMIT_DATA, "data_seg_size_kbytes"),
        (libc::RLIMIT_FSIZE, "file_size_blocks"),
        (libc::RLIMIT_MEMLOCK, "max_locked_memory_bytes"),
        (libc::RLIMIT_RSS, "max_memory_size_kbytes"),
        (libc::RLIMIT_NOFILE, "open_files"),
        (libc::RLIMIT_STACK, "stack_size_bytes"),
        (libc::RLIMIT_CPU, "cpu_time_seconds"),
        (libc::RLIMIT_NPROC, "max_user_processes"),
        (libc::RLIMIT_AS, "virtual_memory_kbytes"),
    ] {
        // SAFETY: an all-zero rlimit is valid, and getrlimit only writes it.
        let mut limit: libc::rlimit = unsafe { core::mem::zeroed() };
        // SAFETY: as above. Node leaves out a limit it cannot read.
        if unsafe { libc::getrlimit(resource, &mut limit) } != 0 {
            continue;
        }
        let shown = |value: libc::rlim_t| {
            if value == libc::RLIM_INFINITY {
                JSValue::from_utf8(b"unlimited")
            } else {
                number(value as f64)
            }
        };
        put(&limits, label, object_of(global, &[("soft", shown(limit.rlim_cur)), ("hard", shown(limit.rlim_max))]));
    }

    Ok(object_of(
        global,
        &[
            ("header", header.to_js()),
            ("javascriptStack", stack.to_js()),
            ("javascriptHeap", heap.to_js()),
            ("nativeStack", empty_array()),
            ("resourceUsage", resource),
            ("uvthreadResourceUsage", thread),
            ("libuv", empty_array()),
            ("workers", empty_array()),
            ("environmentVariables", of_process("env")?),
            ("userLimits", limits.to_js()),
            ("sharedObjects", empty_array()),
            ("cpus", empty_array()),
            ("networkInterfaces", empty_array()),
        ],
    ))
}

/// The functions, by the name js/bun.ts reads them under, and `execPath`.
pub fn functions(global: &JSGlobalObject) -> Vec<(&'static str, JSValue)> {
    vec![
        ("umask", JSFunction::create(global, "umask", umask, 1)),
        ("hrtime", JSFunction::create(global, "hrtime", hrtime, 1)),
        ("hrtimeBigint", JSFunction::create(global, "bigint", hrtime_bigint, 0)),
        ("memoryUsage", JSFunction::create(global, "memoryUsage", memory_usage, 0)),
        ("memoryUsageRss", JSFunction::create(global, "rss", memory_usage_rss, 0)),
        ("cpuUsage", JSFunction::create(global, "cpuUsage", cpu_usage, 1)),
        ("resourceUsage", JSFunction::create(global, "resourceUsage", resource_usage, 0)),
        ("getReport", JSFunction::create(global, "getReport", get_report, 0)),
        ("execPath", JSValue::from_utf8(&exec_path())),
    ]
    .into_iter()
    .chain(super::process_kill::functions(global))
    .collect()
}
