//! The functions of `Bun` that src/jsc/bindings/BunObject.cpp and src/runtime/api/BunObject.rs define, on `bun_js`.

use bun_js::{CallFrame, DeepEqualsMode, ErrorCode, JSFunction, JSGlobalObject, JSValue, JsResult};

/// `Bun.deepEquals(a, b, strict?, skipPrototype?)`, as C++'s `functionBunDeepEquals`.
pub fn deep_equals(global: &JSGlobalObject, frame: &CallFrame<'_>) -> JsResult<JSValue> {
    if frame.arguments_count() < 2 {
        return Err(global.throw_type_error(format_args!("Expected 2 values to compare")));
    }
    let is_true = |value: JSValue| value.is_boolean() && value.as_boolean();
    let strict = is_true(frame.argument(2));
    let mode = DeepEqualsMode {
        strict,
        skip_prototype_identity: strict && is_true(frame.argument(3)),
    };
    let equal = frame
        .argument(0)
        .deep_equals_with(&frame.argument(1), global, mode)?;
    Ok(JSValue::js_boolean(equal))
}

/// `Bun.sleepSync(milliseconds)`, as BunObject.rs's `sleep_sync`: beyond `i32::MAX` it is `i32::MAX`.
pub fn sleep_sync(global: &JSGlobalObject, frame: &CallFrame<'_>) -> JsResult<JSValue> {
    let value = frame.argument(0);
    if !value.is_number() {
        return Err(ErrorCode::INVALID_ARG_TYPE.throw(global, format_args!("milliseconds must be a number")));
    }
    let milliseconds = value.as_number() as i32;
    let Ok(duration) = u64::try_from(milliseconds) else {
        return Err(ErrorCode::INVALID_ARG_TYPE.throw(
            global,
            format_args!("argument to sleepSync must not be negative, got {milliseconds}"),
        ));
    };
    std::thread::sleep(core::time::Duration::from_millis(duration));
    Ok(JSValue::UNDEFINED)
}

/// A string member of `Bun.which`'s options, as a loose string: `None` where it is absent.
fn which_option(global: &JSGlobalObject, options: &JSValue, key: &str) -> JsResult<Option<Vec<u8>>> {
    if !options.is_object() {
        return Ok(None);
    }
    match options.get(global, key)? {
        Some(value) if !value.is_undefined() => Ok(Some(value.to_utf8(global)?.into_bytes())),
        _ => Ok(None),
    }
}

/// `Bun.which(command, { PATH, cwd })`, as BunObject.rs's `which`: the path of the executable, or `null`.
pub fn which(global: &JSGlobalObject, frame: &CallFrame<'_>) -> JsResult<JSValue> {
    let command = frame.argument(0);
    let command = if command.is_undefined_or_null() {
        Vec::new()
    } else {
        command.to_utf8(global)?.into_bytes()
    };
    if command.len() >= bun_core::MAX_PATH_BYTES {
        return Err(global.throw(format_args!("bin path is too long")));
    }
    let options = frame.argument(1);
    let path = match which_option(global, &options, "PATH")? {
        Some(path) => path,
        None => std::env::var_os("PATH").map(|path| path.into_encoded_bytes()).unwrap_or_default(),
    };
    let cwd = match which_option(global, &options, "cwd")? {
        Some(cwd) => cwd,
        None => std::env::current_dir()
            .map(|dir| dir.into_os_string().into_encoded_bytes())
            .unwrap_or_default(),
    };
    Ok(match bun_which::which(&path, &cwd, &command) {
        Some(bin) => JSValue::from_utf8(bin.as_bytes()),
        None => JSValue::NULL,
    })
}

/// Each function by the name `Bun` has it under.
pub fn functions(global: &JSGlobalObject) -> Vec<(&'static str, JSValue)> {
    vec![
        ("deepEquals", JSFunction::create(global, "deepEquals", deep_equals, 2)),
        ("sleepSync", JSFunction::create(global, "sleepSync", sleep_sync, 1)),
        ("which", JSFunction::create(global, "which", which, 1)),
    ]
}
