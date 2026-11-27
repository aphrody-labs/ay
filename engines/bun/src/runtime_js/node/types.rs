//! What node:fs reads its arguments as: src/runtime/node/types.rs and time_like.rs on `bun_js`. A path, a string or
//! the bytes of a buffer are copied out of the value, so that what an operation holds is its own and can go to the
//! work pool.

use core::fmt;

use bun_core::{ZBox, strings};
use bun_js::{ErrorCode, JSGlobalObject, JSValue, JsError, JsResult};
use bun_sys::{Fd, Mode, O};

// ───────────────────────────── errors ─────────────────────────────

/// `Bun.inspect` on one line, as `JSGlobalObject::inspect_for_error_message`.
pub(crate) fn inspect(global: &JSGlobalObject, value: &JSValue) -> JsResult<String> {
    if value.is_string() {
        let text = value.to_utf8(global)?;
        return Ok(if text.contains('\'') {
            format!("\"{text}\"")
        } else {
            format!("'{text}'")
        });
    }
    let inspect = match global.to_js_value().get(global, "Bun")? {
        Some(bun) => bun.get(global, "inspect")?,
        None => None,
    };
    let Some(inspect) = inspect.filter(JSValue::is_callable) else {
        return value.to_utf8(global);
    };
    inspect
        .call(global, JSValue::UNDEFINED, &[value.clone()])?
        .to_utf8(global)
}

/// `ERR_INVALID_ARG_TYPE`: `The "name" argument must be of type <expected>. Received <what value is>`.
pub(crate) fn invalid_arg_type(
    global: &JSGlobalObject,
    name: &str,
    expected: &str,
    value: &JSValue,
) -> JsError {
    let received = match bun_js::bindgen::determine_specific_type(global, value) {
        Ok(received) => received,
        Err(error) => return error,
    };
    let noun = if name.contains('.') {
        "property"
    } else {
        "argument"
    };
    ErrorCode::INVALID_ARG_TYPE.throw(
        global,
        format_args!("The \"{name}\" {noun} must be {expected}. Received {received}"),
    )
}

/// `throw_invalid_arguments`: `ERR_INVALID_ARG_TYPE` with the message as it is.
pub(crate) fn invalid_arguments(global: &JSGlobalObject, args: fmt::Arguments<'_>) -> JsError {
    ErrorCode::INVALID_ARG_TYPE.throw(global, args)
}

pub(crate) fn invalid_arg_value(global: &JSGlobalObject, args: fmt::Arguments<'_>) -> JsError {
    ErrorCode::INVALID_ARG_VALUE.throw(global, args)
}

/// Node's `addNumericalSeparator` for what `ERR_OUT_OF_RANGE` received.
fn received_number(global: &JSGlobalObject, value: &JSValue) -> JsResult<String> {
    if !value.is_number() {
        return inspect(global, value);
    }
    let n = value.as_number();
    if n == 0.0 && n.is_sign_negative() {
        return Ok("-0".into());
    }
    let text = value.to_utf8(global)?;
    if n.fract() != 0.0 || n.abs() <= 4_294_967_296.0 || !n.is_finite() || text.contains('e') {
        return Ok(text);
    }
    let (sign, digits) = text.strip_prefix('-').map_or(("", &text[..]), |d| ("-", d));
    let mut groups = Vec::new();
    let mut end = digits.len();
    while end > 3 {
        groups.push(&digits[end - 3..end]);
        end -= 3;
    }
    groups.push(&digits[..end]);
    groups.reverse();
    Ok(format!("{sign}{}", groups.join("_")))
}

/// `ERR_OUT_OF_RANGE`: `The value of "name" is out of range. It must be <range>. Received <value>`.
pub(crate) fn out_of_range(
    global: &JSGlobalObject,
    name: &str,
    range: &str,
    value: &JSValue,
) -> JsError {
    let received = match received_number(global, value) {
        Ok(received) => received,
        Err(error) => return error,
    };
    ErrorCode::OUT_OF_RANGE.throw(
        global,
        format_args!("The value of \"{name}\" is out of range. It must be {range}. Received {received}"),
    )
}

/// A `bun_sys::Error` as Node's system error (`err.to_js(global)` of node_fs.rs).
pub(crate) fn sys_error_to_js(global: &JSGlobalObject, err: &bun_sys::Error) -> JSValue {
    crate::sys_js::SystemError::from(err).to_error_instance(global)
}

// ───────────────────────────── validators ─────────────────────────────

/// Node's `validateInt32`.
pub(crate) fn validate_int32(
    global: &JSGlobalObject,
    value: &JSValue,
    name: &str,
    min: i32,
    max: i32,
) -> JsResult<i32> {
    if !value.is_number() {
        return Err(invalid_arg_type(global, name, "of type number", value));
    }
    let n = value.as_number();
    if n.fract() != 0.0 || !n.is_finite() {
        return Err(out_of_range(global, name, "an integer", value));
    }
    if n < f64::from(min) || n > f64::from(max) {
        return Err(out_of_range(
            global,
            name,
            &format!(">= {min} && <= {max}"),
            value,
        ));
    }
    Ok(n as i32)
}

/// Node's `validateUint32`.
pub(crate) fn validate_uint32(
    global: &JSGlobalObject,
    value: &JSValue,
    name: &str,
    positive: bool,
) -> JsResult<u32> {
    if !value.is_number() {
        return Err(invalid_arg_type(global, name, "of type number", value));
    }
    let n = value.as_number();
    if n.fract() != 0.0 || !n.is_finite() {
        return Err(out_of_range(global, name, "an integer", value));
    }
    let min = u32::from(positive);
    if n < f64::from(min) || n > f64::from(u32::MAX) {
        return Err(out_of_range(
            global,
            name,
            &format!(">= {min} && <= {}", u32::MAX),
            value,
        ));
    }
    Ok(n as u32)
}

/// Node's `validateInteger` (a safe integer, in `[min, max]`).
pub(crate) fn validate_integer(
    global: &JSGlobalObject,
    value: &JSValue,
    name: &str,
    min: f64,
    max: f64,
) -> JsResult<i64> {
    if !value.is_number() {
        return Err(invalid_arg_type(global, name, "of type number", value));
    }
    let n = value.as_number();
    if n.fract() != 0.0 || !n.is_finite() {
        return Err(out_of_range(global, name, "an integer", value));
    }
    if n < min || n > max {
        return Err(out_of_range(
            global,
            name,
            &format!(">= {min} && <= {max}"),
            value,
        ));
    }
    Ok(n as i64)
}

// ───────────────────────────── the arguments ─────────────────────────────

/// `bun_jsc::ArgumentsSlice`: the arguments of a call, read in order. `next` looks, `eat` takes.
pub(crate) struct ArgumentsSlice<'v> {
    values: &'v [JSValue],
    index: usize,
    pub(crate) will_be_async: bool,
    /// An error an async call reports by rejecting, rather than throwing: a path that is too long.
    pub(crate) deferred_error: Option<bun_sys::Error>,
}

impl<'v> ArgumentsSlice<'v> {
    pub(crate) fn new(values: &'v [JSValue], will_be_async: bool) -> Self {
        ArgumentsSlice {
            values,
            index: 0,
            will_be_async,
            deferred_error: None,
        }
    }

    pub(crate) fn next(&self) -> Option<JSValue> {
        self.values.get(self.index).cloned()
    }

    pub(crate) fn eat(&mut self) {
        if self.index < self.values.len() {
            self.index += 1;
        }
    }

    pub(crate) fn next_eat(&mut self) -> Option<JSValue> {
        let value = self.next();
        self.eat();
        value
    }

    /// The next argument, `undefined` past the end, taken.
    pub(crate) fn next_or_undefined(&mut self) -> JSValue {
        self.next_eat().unwrap_or_default()
    }
}

// ───────────────────────────── paths ─────────────────────────────

/// A path, as the bytes the system call takes. A string is UTF-8, a buffer its bytes.
pub(crate) type PathLike = ZBox;

fn no_null_bytes(global: &JSGlobalObject, bytes: &[u8], name: &str, accepts: &str) -> JsResult<()> {
    if strings::index_of_char_usize(bytes, 0).is_none() {
        return Ok(());
    }
    let shown = String::from_utf8_lossy(bytes);
    let quoted = if shown.contains('\'') {
        format!("\"{shown}\"")
    } else {
        format!("'{shown}'")
    };
    Err(invalid_arg_value(
        global,
        format_args!("The argument '{name}' must be {accepts} without null bytes. Received {quoted}"),
    ))
}

/// Node's `isURL`: what has an `href` and a `protocol`, and no `auth` or `path` of a legacy `url.parse` result.
fn is_url(global: &JSGlobalObject, value: &JSValue) -> JsResult<bool> {
    if !value.is_object() {
        return Ok(false);
    }
    let truthy = |key: &str| -> JsResult<bool> { Ok(value.get(global, key)?.is_some_and(|v| v.is_truthy())) };
    Ok(truthy("href")?
        && truthy("protocol")?
        && value.get(global, "auth")?.is_none()
        && value.get(global, "path")?.is_none())
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

fn percent_decode(text: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(text.len());
    let mut i = 0;
    while i < text.len() {
        if text[i] == b'%'
            && let (Some(hi), Some(lo)) = (
                text.get(i + 1).copied().and_then(hex_value),
                text.get(i + 2).copied().and_then(hex_value),
            )
        {
            out.push(hi << 4 | lo);
            i += 3;
            continue;
        }
        out.push(text[i]);
        i += 1;
    }
    out
}

/// Node's `fileURLToPath` on POSIX.
fn file_url_path(global: &JSGlobalObject, url: &JSValue) -> JsResult<Vec<u8>> {
    let read = |key: &str| -> JsResult<String> {
        match url.get(global, key)? {
            Some(value) => value.to_utf8(global),
            None => Ok(String::new()),
        }
    };
    if read("protocol")? != "file:" {
        return Err(ErrorCode::INVALID_URL_SCHEME.throw(global, format_args!("The URL must be of scheme file")));
    }
    if !read("hostname")?.is_empty() {
        return Err(ErrorCode::INVALID_FILE_URL_HOST.throw(
            global,
            format_args!("File URL host must be \"localhost\" or empty on {}", std::env::consts::OS),
        ));
    }
    let pathname = read("pathname")?;
    let bytes = pathname.as_bytes();
    let mut i = 0;
    while let Some(at) = strings::index_of_char_usize(&bytes[i..], b'%') {
        let at = i + at;
        if bytes.get(at + 1) == Some(&b'2') && matches!(bytes.get(at + 2), Some(b'f' | b'F')) {
            return Err(ErrorCode::INVALID_FILE_URL_PATH.throw(
                global,
                format_args!("File URL path must not include encoded / characters"),
            ));
        }
        i = at + 1;
    }
    Ok(percent_decode(bytes))
}

/// The bytes of a `Buffer`, a typed array, a `DataView` or an `ArrayBuffer`, copied.
pub(crate) fn bytes_of_view(global: &JSGlobalObject, value: &JSValue) -> JsResult<Option<Vec<u8>>> {
    value.array_buffer_or_view_bytes(global)
}

/// `PathLike::from_js`: a string, a `String` object, a `Buffer`, a `DataView`, an `ArrayBuffer` or a `file:` URL.
/// `None` (and nothing taken) for anything else.
pub(crate) fn path_like_from_js(
    global: &JSGlobalObject,
    arguments: &mut ArgumentsSlice<'_>,
) -> JsResult<Option<PathLike>> {
    let Some(arg) = arguments.next() else {
        return Ok(None);
    };
    let bytes = if arg.is_string() || arg.is_string_object() {
        let text = arg.to_utf8(global)?.into_bytes();
        no_null_bytes(global, &text, "path", "a string, Uint8Array, or URL")?;
        text
    } else if arg.is_uint8_array_data_view_or_array_buffer() {
        let bytes = arg.array_buffer_or_view_bytes(global)?.unwrap_or_default();
        if bytes.is_empty() {
            return Err(invalid_arguments(global, format_args!("Invalid path buffer: can't be empty")));
        }
        no_null_bytes(global, &bytes, "path", "a string, Uint8Array, or URL")?;
        bytes
    } else if is_url(global, &arg)? {
        let path = file_url_path(global, &arg)?;
        if path.is_empty() {
            return Err(invalid_arg_value(global, format_args!("URL must be a non-empty \"file:\" path")));
        }
        no_null_bytes(global, &path, "path", "a string, Uint8Array, or URL")?;
        path
    } else {
        return Ok(None);
    };
    arguments.eat();
    if bytes.len() >= bun_core::MAX_PATH_BYTES {
        let err = bun_sys::Error::from_code(bun_sys::E::ENAMETOOLONG, bun_sys::Tag::open).with_path(&bytes);
        if !arguments.will_be_async {
            return Err(global.throw_value(sys_error_to_js(global, &err)));
        }
        arguments.deferred_error.get_or_insert(err);
        return Ok(Some(ZBox::default()));
    }
    Ok(Some(ZBox::from_vec(bytes)))
}

/// `PathLike::from_js_required`: `<name> must be a string or TypedArray` for anything else.
pub(crate) fn path_like_required(
    global: &JSGlobalObject,
    arguments: &mut ArgumentsSlice<'_>,
    name: &str,
) -> JsResult<PathLike> {
    path_like_from_js(global, arguments)?
        .ok_or_else(|| invalid_arguments(global, format_args!("{name} must be a string or TypedArray")))
}

/// A path or a file descriptor.
pub(crate) enum PathOrFd {
    Path(PathLike),
    Fd(Fd),
}

/// `Fd::from_js_validated`: `None` for what is not a number; a number that is not an fd throws.
pub(crate) fn fd_from_js_validated(global: &JSGlobalObject, value: &JSValue) -> JsResult<Option<Fd>> {
    if !value.is_number() {
        return Ok(None);
    }
    let float = value.as_number();
    if float.fract() != 0.0 || !float.is_finite() {
        return Err(out_of_range(global, "fd", "an integer", value));
    }
    if float < 0.0 || float > f64::from(i32::MAX) {
        return Err(out_of_range(global, "fd", &format!(">= 0 && <= {}", i32::MAX), value));
    }
    Ok(Some(Fd::from_uv(float as i32)))
}

/// `FdArgExt::from_js_required`.
pub(crate) fn fd_required(global: &JSGlobalObject, arguments: &mut ArgumentsSlice<'_>) -> JsResult<Fd> {
    let value = arguments.next_or_undefined();
    fd_from_js_validated(global, &value)?.ok_or_else(|| invalid_arg_type(global, "fd", "of type number", &value))
}

pub(crate) fn path_or_fd_from_js(
    global: &JSGlobalObject,
    arguments: &mut ArgumentsSlice<'_>,
) -> JsResult<Option<PathOrFd>> {
    let Some(first) = arguments.next() else {
        return Ok(None);
    };
    if let Some(fd) = fd_from_js_validated(global, &first)? {
        arguments.eat();
        return Ok(Some(PathOrFd::Fd(fd)));
    }
    Ok(path_like_from_js(global, arguments)?.map(PathOrFd::Path))
}

// ───────────────────────────── encodings ─────────────────────────────

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Encoding {
    Utf8,
    Utf16le,
    Latin1,
    Ascii,
    Base64,
    Base64url,
    Hex,
    Buffer,
}

/// A string of Latin-1 code units.
fn latin1(units: &[u8]) -> JSValue {
    JSValue::from_utf16(&units.iter().map(|&unit| u16::from(unit)).collect::<Vec<u16>>())
}

impl Encoding {
    pub(crate) fn from(name: &[u8]) -> Option<Encoding> {
        let lower = name.to_ascii_lowercase();
        Some(match &lower[..] {
            b"hex" => Encoding::Hex,
            b"utf8" | b"utf-8" => Encoding::Utf8,
            b"ucs2" | b"ucs-2" | b"utf16le" | b"utf-16le" | b"utf16-le" => Encoding::Utf16le,
            b"ascii" => Encoding::Ascii,
            b"base64" => Encoding::Base64,
            b"binary" | b"latin1" => Encoding::Latin1,
            b"buffer" => Encoding::Buffer,
            b"base64url" => Encoding::Base64url,
            _ => return None,
        })
    }

    fn throw_error(global: &JSGlobalObject, value: &JSValue) -> JsError {
        let shown = value.to_utf8(global).unwrap_or_default();
        invalid_arg_value(global, format_args!("encoding '{shown}' is an invalid encoding"))
    }

    /// `Encoding::assert`: a falsy value is `default`, a string that names none throws.
    pub(crate) fn assert(global: &JSGlobalObject, value: &JSValue, default: Encoding) -> JsResult<Encoding> {
        if value.is_falsey() {
            return Ok(default);
        }
        if !value.is_string() {
            return Err(Self::throw_error(global, value));
        }
        let name = value.to_utf8(global)?;
        if name.is_empty() {
            return Ok(default);
        }
        Self::from(name.as_bytes()).ok_or_else(|| Self::throw_error(global, value))
    }

    /// `encoding` of an options argument that is a string or an object with an `encoding`.
    /// `encoding.encode(bytes)`: a string, or a `Buffer` for `"buffer"`.
    pub(crate) fn to_js(self, global: &JSGlobalObject, bytes: &[u8]) -> JsResult<JSValue> {
        Ok(match self {
            Encoding::Buffer => JSValue::create_buffer(global, bytes)?,
            Encoding::Utf8 => JSValue::from_utf8(bytes),
            Encoding::Latin1 => latin1(bytes),
            Encoding::Ascii => latin1(&bytes.iter().map(|b| b & 0x7f).collect::<Vec<u8>>()),
            Encoding::Utf16le => JSValue::from_utf16(
                &bytes
                    .chunks_exact(2)
                    .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
                    .collect::<Vec<u16>>(),
            ),
            Encoding::Hex => {
                const DIGITS: &[u8; 16] = b"0123456789abcdef";
                let mut out = Vec::with_capacity(bytes.len() * 2);
                for byte in bytes {
                    out.push(DIGITS[usize::from(byte >> 4)]);
                    out.push(DIGITS[usize::from(byte & 15)]);
                }
                latin1(&out)
            }
            Encoding::Base64 => latin1(&bun_base64::encode_alloc(bytes)),
            Encoding::Base64url => {
                let mut out = vec![0; bytes.len().div_ceil(3) * 4];
                let len = bun_base64::encode_url_safe(&mut out, bytes);
                out.truncate(len);
                while out.last() == Some(&b'=') {
                    out.pop();
                }
                latin1(&out)
            }
        })
    }

    /// The bytes of `text` in this encoding, as `Buffer.from(text, encoding)`.
    pub(crate) fn decode(self, global: &JSGlobalObject, text: &JSValue) -> JsResult<Vec<u8>> {
        Ok(match self {
            Encoding::Utf8 | Encoding::Buffer => text.to_utf8(global)?.into_bytes(),
            Encoding::Latin1 | Encoding::Ascii => text.to_utf16(global)?.iter().map(|&u| u as u8).collect(),
            Encoding::Utf16le => text
                .to_utf16(global)?
                .iter()
                .flat_map(|u| u.to_le_bytes())
                .collect(),
            Encoding::Hex => {
                let text = text.to_utf8(global)?;
                let mut out = Vec::with_capacity(text.len() / 2);
                for pair in text.as_bytes().chunks_exact(2) {
                    match (hex_value(pair[0]), hex_value(pair[1])) {
                        (Some(hi), Some(lo)) => out.push(hi << 4 | lo),
                        _ => break,
                    }
                }
                out
            }
            Encoding::Base64 | Encoding::Base64url => {
                let text = text.to_utf8(global)?;
                let mut out = vec![0; bun_base64::decode_len(text.as_bytes()) + 3];
                let len = bun_base64::decode_lenient(&mut out, text.as_bytes(), self == Encoding::Base64url);
                out.truncate(len);
                out
            }
        })
    }
}

// ───────────────────────────── modes, flags, times ─────────────────────────────

/// `mode_from_js`: a number, or a string of octal digits.
pub(crate) fn mode_from_js(global: &JSGlobalObject, value: &JSValue) -> JsResult<Option<Mode>> {
    if value.is_number() {
        return Ok(Some(validate_uint32(global, value, "mode", false)? as Mode));
    }
    if value.is_undefined_or_null() {
        return Ok(None);
    }
    if !value.is_string() {
        return Err(invalid_arg_type(global, "mode", "of type number", value));
    }
    let text = value.to_utf8(global)?;
    if text.is_empty() || !text.bytes().all(|b| (b'0'..=b'7').contains(&b)) {
        let actual = inspect(global, value)?;
        return Err(invalid_arg_value(
            global,
            format_args!(
                "The argument 'mode' must be a 32-bit unsigned integer or an octal string. Received {actual}"
            ),
        ));
    }
    let parsed = u64::from_str_radix(&text, 8).unwrap_or(u64::MAX);
    let parsed = JSValue::js_number_from_uint64(parsed);
    Ok(Some(validate_uint32(global, &parsed, "mode", false)? as Mode))
}

fn flag_of(name: &[u8]) -> Option<i32> {
    Some(match name {
        b"r" => O::RDONLY,
        b"w" => O::TRUNC | O::CREAT | O::WRONLY,
        b"a" => O::APPEND | O::CREAT | O::WRONLY,
        b"r+" => O::RDWR,
        b"w+" => O::TRUNC | O::CREAT | O::RDWR,
        b"a+" => O::APPEND | O::CREAT | O::RDWR,
        b"rs" | b"sr" => O::RDONLY | O::SYNC,
        b"wx" | b"xw" => O::TRUNC | O::CREAT | O::WRONLY | O::EXCL,
        b"ax" | b"xa" => O::APPEND | O::CREAT | O::WRONLY | O::EXCL,
        b"as" | b"sa" => O::APPEND | O::CREAT | O::WRONLY | O::SYNC,
        b"rs+" | b"sr+" => O::RDWR | O::SYNC,
        b"wx+" | b"xw+" => O::TRUNC | O::CREAT | O::RDWR | O::EXCL,
        b"ax+" | b"xa+" => O::APPEND | O::CREAT | O::RDWR | O::EXCL,
        b"as+" | b"sa+" => O::APPEND | O::CREAT | O::RDWR | O::SYNC,
        _ => return None,
    })
}

/// `FileSystemFlags::from_js`: Node's `stringToFlags`.
pub(crate) fn flags_from_js(global: &JSGlobalObject, value: &JSValue) -> JsResult<Option<i32>> {
    if value.is_number() {
        return Ok(Some(validate_int32(global, value, "flags", i32::MIN, i32::MAX)?.max(0)));
    }
    if value.is_undefined_or_null() {
        return Ok(None);
    }
    if value.is_string() {
        if let Some(flags) = flag_of(value.to_utf8(global)?.as_bytes()) {
            return Ok(Some(flags));
        }
    }
    let actual = inspect(global, value)?;
    Err(invalid_arg_value(global, format_args!("The argument 'flags' is invalid. Received {actual}")))
}

/// `FileSystemFlags::from_js_number_only`: the mode of `access` and `copyFile`, `0..=7`.
pub(crate) fn mode_number_only(global: &JSGlobalObject, value: &JSValue) -> JsResult<i32> {
    if !value.is_number() {
        if value.is_undefined_or_null() {
            return Ok(0);
        }
        return Err(invalid_arguments(global, format_args!("mode must be int32 or null/undefined")));
    }
    let float = value.as_number();
    if !float.is_finite() || !(0.0..=7.0).contains(&float) {
        return Err(ErrorCode::OUT_OF_RANGE.throw(global, format_args!("mode is out of range: >= 0 and <= 7")));
    }
    Ok(float as i32)
}

/// `time_like::from_js`: seconds as a number or a numeric string, or a `Date`. A negative number is now.
pub(crate) fn time_like_from_js(global: &JSGlobalObject, value: &JSValue) -> JsResult<Option<bun_sys::TimeLike>> {
    let from_seconds = |seconds: f64| {
        let mut sec = seconds.div_euclid(1.0);
        let mut nsec = seconds.rem_euclid(1.0) * 1e9;
        if nsec >= 1e9 {
            nsec -= 1e9;
            sec += 1.0;
        }
        bun_sys::TimeLike {
            sec: sec as i64,
            nsec: nsec as i64,
        }
    };
    if value.is_number() {
        let seconds = value.as_number();
        if !seconds.is_finite() {
            return Ok(None);
        }
        if seconds < 0.0 {
            return Ok(Some(bun_sys::TimeLike {
                sec: 0,
                nsec: libc::UTIME_NOW,
            }));
        }
        return Ok(Some(from_seconds(seconds)));
    }
    if let Some(ms) = value.as_date_ms(global)? {
        if ms.is_finite() {
            return Ok(Some(bun_sys::TimeLike {
                sec: ms.div_euclid(1000.0) as i64,
                nsec: (ms.rem_euclid(1000.0) * 1e6) as i64,
            }));
        }
        return Ok(None);
    }
    if value.is_string() {
        let seconds = value.to_number(global)?;
        if seconds.is_finite() {
            return Ok(Some(from_seconds(seconds)));
        }
    }
    Ok(None)
}
