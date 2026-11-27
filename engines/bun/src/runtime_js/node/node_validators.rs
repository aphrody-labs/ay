//! The bodies of src/runtime/node/node_validators.bind.ts on `bun_js`: src/jsc/bindings/NodeValidatorBodies.h and
//! the `Bun::V` validators of NodeValidator.cpp, with the messages that src/jsc/bindings/ErrorCode.cpp words.

use bun_js::bindgen::{self, JsHeap, JsLocal, JsScope};
use bun_js::{ErrorCode, JSGlobalObject, JSValue, JsResult};

type Value<'a> = JsLocal<'a>;
type Unit<'a> = bindgen::Result<'a, ()>;

const MIN_SAFE_INTEGER: f64 = -9_007_199_254_740_991.0;
const MAX_SAFE_INTEGER: f64 = 9_007_199_254_740_991.0;

// ───────────────────────────── how ErrorCode.cpp words a value ─────────────────────────────

/// A number as `String(n)` writes it, as WTF's `append(double)` does.
fn number_text(global: &JSGlobalObject, n: f64) -> JsResult<String> {
    JSValue::js_number(n).to_utf8(global)
}

/// `Bun.inspect` on one line, as `Bun__inspect_singleline`.
fn inspect(global: &JSGlobalObject, value: &JSValue) -> JsResult<String> {
    let bun = global.to_js_value().get(global, "Bun")?;
    let inspect = match &bun {
        Some(bun) => bun.get(global, "inspect")?,
        None => None,
    };
    let Some(inspect) = inspect.filter(JSValue::is_callable) else {
        return value.to_utf8(global);
    };
    let options = JSValue::create_empty_object(global, 2);
    options.put(global, "compact", JSValue::js_boolean(true));
    options.put(global, "breakLength", JSValue::js_number(f64::INFINITY));
    inspect.call(global, JSValue::UNDEFINED, &[value.clone(), options])?.to_utf8(global)
}

fn function_name(global: &JSGlobalObject, value: &JSValue) -> JsResult<String> {
    Ok(match value.get(global, "name")? {
        Some(name) if name.is_string() => name.to_utf8(global)?,
        _ => String::new(),
    })
}

/// `JSValueToStringSafe`: a string quoted as `inspect` quotes it where `quotes`, a symbol and a function described,
/// anything else inspected.
fn to_string_safe(global: &JSGlobalObject, value: &JSValue, quotes: bool) -> JsResult<String> {
    if value.is_string() {
        let text = value.to_utf8(global)?;
        if !quotes {
            return Ok(text);
        }
        let quote = if text.contains('\'') { '"' } else { '\'' };
        let mut out = String::with_capacity(text.len() + 2);
        out.push(quote);
        for c in text.chars() {
            match c {
                '\n' => out.push_str("\\n"),
                '\t' => out.push_str("\\t"),
                '\r' => out.push_str("\\r"),
                '\\' => out.push_str("\\\\"),
                c if c == quote => {
                    out.push('\\');
                    out.push(c);
                }
                c => out.push(c),
            }
        }
        out.push(quote);
        return Ok(out);
    }
    if value.is_symbol() {
        return Ok(format!("Symbol({})", value.symbol_description().unwrap_or_default()));
    }
    if value.is_callable() {
        let name = function_name(global, value)?;
        return Ok(if name.is_empty() { "[Function (anonymous)]".into() } else { format!("[Function: {name}]") });
    }
    if value.is_number() && value.as_number() == 0.0 && value.as_number().is_sign_negative() {
        return Ok("-0".into());
    }
    if !value.is_object() {
        return value.to_utf8(global);
    }
    inspect(global, value)
}

/// `determineSpecificType`, after Node's lib/internal/errors.js.
fn specific_type(global: &JSGlobalObject, value: &JSValue) -> JsResult<String> {
    if value.is_null() {
        return Ok("null".into());
    }
    if value.is_undefined() {
        return Ok("undefined".into());
    }
    if value.is_number() {
        let n = value.as_number();
        return Ok(if n.is_nan() {
            "type number (NaN)".into()
        } else if n.is_infinite() {
            format!("type number ({}Infinity)", if n < 0.0 { "-" } else { "" })
        } else {
            format!("type number ({})", number_text(global, n)?)
        });
    }
    if value.is_boolean() {
        return Ok(format!("type boolean ({})", value.as_boolean()));
    }
    if value.is_big_int() {
        return Ok(format!("type bigint ({}n)", value.to_utf8(global)?));
    }
    if value.is_symbol() {
        return Ok(format!("type symbol (Symbol({}))", value.symbol_description().unwrap_or_default()));
    }
    if value.is_callable() {
        return Ok(format!("function {}", function_name(global, value)?));
    }
    if value.is_string() {
        let text: Vec<char> = value.to_utf8(global)?.chars().collect();
        let ellipsis = text.len() > 28;
        let shown: String = text.iter().take(if ellipsis { 25 } else { text.len() }).collect();
        let suffix = if ellipsis { "..." } else { "" };
        return Ok(if shown.contains('\'') || text.contains(&'\'') {
            format!("type string (\"{}{suffix}\")", shown.replace('"', "\\\""))
        } else {
            format!("type string ('{shown}{suffix}')")
        });
    }
    if let Some(constructor) = value.get(global, "constructor")?
        && constructor.to_boolean()
    {
        let name = constructor.get(global, "name")?.unwrap_or_default();
        return Ok(format!("an instance of {}", name.to_utf8(global)?));
    }
    to_string_safe(global, value, false)
}

/// Node's `addNumericalSeparator`: `1_000_000`.
fn numerical_separator(digits: &str) -> String {
    let (sign, digits) = digits.strip_prefix('-').map_or(("", digits), |rest| ("-", rest));
    let mut groups = Vec::new();
    let mut end = digits.len();
    while end > 3 {
        groups.push(&digits[end - 3..end]);
        end -= 3;
    }
    groups.push(&digits[..end]);
    groups.reverse();
    format!("{sign}{}", groups.join("_"))
}

/// `appendOutOfRangeReceived`.
fn out_of_range_received(global: &JSGlobalObject, input: &JSValue) -> JsResult<String> {
    if input.is_number() {
        let n = input.as_number();
        if n.is_finite() && n.trunc() == n && n.abs() > 4_294_967_296.0 {
            return Ok(numerical_separator(&number_text(global, n)?));
        }
        if n == 0.0 && n.is_sign_negative() {
            return Ok("-0".into());
        }
    } else if input.is_big_int() {
        let digits = input.to_utf8(global)?;
        let magnitude = digits.trim_start_matches('-');
        let beyond_32_bits = magnitude.len() > 10 || (magnitude.len() == 10 && magnitude > "4294967296");
        return Ok(format!("{}n", if beyond_32_bits { numerical_separator(&digits) } else { digits }));
    }
    to_string_safe(global, input, false)
}

/// Message::addParameter: `"name" argument`, or `property` for a dotted name.
fn parameter(name: &str) -> String {
    if name.ends_with(" argument") {
        name.to_owned()
    } else {
        format!("\"{name}\" {}", if name.contains('.') { "property" } else { "argument" })
    }
}

// ───────────────────────────── the errors of ErrorCode.cpp ─────────────────────────────

/// `ERR::INVALID_ARG_TYPE(name, expected, value)` with one expected type.
fn invalid_arg_type<'a>(heap: &JsHeap<'a>, name: &JSValue, expected: &str, value: &JSValue) -> bindgen::Error<'a> {
    let global = heap.global_object();
    let message = (|| -> JsResult<String> {
        let name = name.to_utf8(global)?;
        let phrase = expected.contains(' ') && !expected.contains(", ") && !expected.contains(" or ");
        let expected = if phrase {
            format!("{}{expected}", if expected.bytes().any(|c| c.is_ascii_uppercase()) { "an " } else { "" })
        } else {
            format!("of type {expected}")
        };
        Ok(format!("The {} must be {expected}. Received {}", parameter(&name), specific_type(global, value)?))
    })();
    match message {
        Ok(message) => heap.error_with_code(ErrorCode::INVALID_ARG_TYPE, format_args!("{message}")),
        Err(error) => error.into(),
    }
}

/// `ERR::INVALID_ARG_INSTANCE`.
fn invalid_arg_instance<'a>(heap: &JsHeap<'a>, name: &str, expected: &str, value: &JSValue) -> bindgen::Error<'a> {
    let kind = if name.contains('.') { "property" } else { "argument" };
    match specific_type(heap.global_object(), value) {
        Ok(received) => heap.error_with_code(
            ErrorCode::INVALID_ARG_TYPE,
            format_args!("The \"{name}\" {kind} must be an instance of {expected}. Received {received}"),
        ),
        Err(error) => error.into(),
    }
}

/// `ERR::OUT_OF_RANGE(name, range, value)`: `It must be <range>`.
fn out_of_range<'a>(heap: &JsHeap<'a>, name: &JSValue, range: &str, value: &JSValue) -> bindgen::Error<'a> {
    let global = heap.global_object();
    let message = (|| -> JsResult<String> {
        Ok(format!(
            "The value of \"{}\" is out of range. It must be {range}. Received {}",
            name.to_utf8(global)?,
            out_of_range_received(global, value)?
        ))
    })();
    match message {
        Ok(message) => heap.error_with_code(ErrorCode::OUT_OF_RANGE, format_args!("{message}")),
        Err(error) => error.into(),
    }
}

/// `conjunctiveRange`: `>= min && <= max`.
fn conjunctive_range(global: &JSGlobalObject, min: f64, max: f64) -> JsResult<String> {
    Ok(format!(">= {} && <= {}", number_text(global, min)?, number_text(global, max)?))
}

/// `ERR::INVALID_ARG_VALUE(name, value, reason)`: the name described as `determineSpecificType` does.
fn invalid_arg_value<'a>(heap: &JsHeap<'a>, name: &JSValue, value: &JSValue, reason: &str) -> bindgen::Error<'a> {
    let global = heap.global_object();
    let message = (|| -> JsResult<String> {
        Ok(format!(
            "The argument '{}' {reason}. Received {}",
            specific_type(global, name)?,
            to_string_safe(global, value, true)?
        ))
    })();
    match message {
        Ok(message) => heap.error_with_code(ErrorCode::INVALID_ARG_VALUE, format_args!("{message}")),
        Err(error) => error.into(),
    }
}

// ───────────────────────────── NodeValidatorBodies.h ─────────────────────────────

/// `boundOr`: the bound, unless finding it out takes a conversion.
fn bound_or(bound: &JSValue, otherwise: f64) -> Option<f64> {
    if bound.is_undefined() {
        Some(otherwise)
    } else if bound.is_number() {
        Some(bound.as_number())
    } else {
        None
    }
}

fn is_integer_between(value: f64, min: f64, max: f64) -> bool {
    value.trunc() == value && value >= min && value <= max
}

/// `V::validateInteger` and `V::validateInt32`: a number, an integer, between `min` and `max`.
fn validate_integer_in<'a>(heap: &JsHeap<'a>, value: &JSValue, name: &JSValue, min: f64, max: f64) -> Unit<'a> {
    let global = heap.global_object();
    if !value.is_number() {
        return Err(invalid_arg_type(heap, name, "number", value));
    }
    let n = value.as_number();
    if n % 1.0 != 0.0 {
        return Err(out_of_range(heap, name, "an integer", value));
    }
    if n < min || n > max {
        return Err(out_of_range(heap, name, &conjunctive_range(global, min, max)?, value));
    }
    Ok(())
}

pub(crate) fn validate_integer<'a>(cx: &mut JsScope<'a>, value: Value<'a>, name: Value<'a>, min: Value<'a>, max: Value<'a>) -> Unit<'a> {
    let (value, name, min, max) = (value.to_js(), name.to_js(), min.to_js(), max.to_js());
    if value.is_number() {
        let n = value.as_number();
        if let (Some(lowest), Some(highest)) = (bound_or(&min, MIN_SAFE_INTEGER), bound_or(&max, MAX_SAFE_INTEGER))
            && is_integer_between(n, lowest, highest)
            && n.is_finite()
        {
            return Ok(());
        }
    }
    let heap = &cx.heap;
    let global = heap.global_object();
    if !value.is_number() {
        return Err(invalid_arg_type(heap, &name, "number", &value));
    }
    let min = if min.is_undefined() { MIN_SAFE_INTEGER } else { min.to_number(global)? };
    let max = if max.is_undefined() { MAX_SAFE_INTEGER } else { max.to_number(global)? };
    validate_integer_in(heap, &value, &name, min, max)
}

pub(crate) fn validate_number<'a>(cx: &mut JsScope<'a>, value: Value<'a>, name: Value<'a>, min: Value<'a>, max: Value<'a>) -> Unit<'a> {
    let (value, name, min, max) = (value.to_js(), name.to_js(), min.to_js(), max.to_js());
    if value.is_number() {
        let n = value.as_number();
        let above = min.is_undefined_or_null() || (min.is_number() && n >= min.as_number());
        let below = max.is_undefined_or_null() || (max.is_number() && n <= max.as_number());
        if above && below {
            return Ok(());
        }
    }
    let heap = &cx.heap;
    let global = heap.global_object();
    if !value.is_number() {
        return Err(invalid_arg_type(heap, &name, "number", &value));
    }
    let n = value.as_number();
    let min_num = min.to_number(global)?;
    let max_num = max.to_number(global)?;
    let (has_min, has_max) = (!min.is_undefined_or_null(), !max.is_undefined_or_null());
    if (has_min && n < min_num) || (has_max && n > max_num) || ((has_min || has_max) && n.is_nan()) {
        let range = match (has_min, has_max) {
            (true, true) => conjunctive_range(global, min_num, max_num)?,
            (true, false) => format!(">= {}", number_text(global, min_num)?),
            (false, true) => format!("<= {}", number_text(global, max_num)?),
            (false, false) => String::new(),
        };
        return Err(out_of_range(heap, &name, &range, &value));
    }
    Ok(())
}

pub(crate) fn validate_int32<'a>(cx: &mut JsScope<'a>, value: Value<'a>, name: Value<'a>, min: Value<'a>, max: Value<'a>) -> Unit<'a> {
    let (value, name, min, max) = (value.to_js(), name.to_js(), min.to_js(), max.to_js());
    if value.is_int32() && min.is_undefined() && max.is_undefined() {
        return Ok(());
    }
    if value.is_number() {
        let n = value.as_number();
        let passes = if min.is_undefined() && max.is_undefined() {
            is_integer_between(n, f64::from(i32::MIN), f64::from(i32::MAX))
        } else if let (Some(lowest), Some(highest)) =
            (bound_or(&min, f64::from(i32::MIN)), bound_or(&max, f64::from(i32::MAX)))
        {
            is_integer_between(n, lowest, highest) && n.is_finite()
        } else {
            false
        };
        if passes {
            return Ok(());
        }
    }
    let heap = &cx.heap;
    let global = heap.global_object();
    if !value.is_number() {
        return Err(invalid_arg_type(heap, &name, "number", &value));
    }
    let min = if min.is_undefined() { f64::from(i32::MIN) } else { min.to_number(global)? };
    let max = if max.is_undefined() { f64::from(i32::MAX) } else { max.to_number(global)? };
    validate_integer_in(heap, &value, &name, min, max)
}

pub(crate) fn validate_uint32<'a>(cx: &mut JsScope<'a>, value: Value<'a>, name: Value<'a>, positive: Value<'a>) -> Unit<'a> {
    let (value, name, positive) = (value.to_js(), name.to_js(), positive.to_js());
    let positive = !positive.is_undefined() && positive.to_boolean();
    let min = if positive { 1.0 } else { 0.0 };
    if value.is_number() && is_integer_between(value.as_number(), min, f64::from(u32::MAX)) {
        return Ok(());
    }
    validate_integer_in(&cx.heap, &value, &name, min, f64::from(u32::MAX))
}

/// `V::validateFiniteNumber`: `false` for `undefined` and `NaN`, `true` for a finite number.
fn validate_finite<'a>(heap: &JsHeap<'a>, number: &JSValue, name: &JSValue) -> bindgen::Result<'a, bool> {
    if number.is_undefined() {
        return Ok(false);
    }
    if number.is_number() {
        let n = number.as_number();
        if n.is_nan() {
            return Ok(false);
        }
        if n.is_finite() {
            return Ok(true);
        }
        return Err(out_of_range(heap, name, "a finite number", number));
    }
    Err(invalid_arg_type(heap, name, "number", number))
}

pub(crate) fn validate_finite_number<'a>(cx: &mut JsScope<'a>, number: Value<'a>, name: Value<'a>) -> bindgen::Result<'a, bool> {
    validate_finite(&cx.heap, &number.to_js(), &name.to_js())
}

pub(crate) fn check_ranges_or_get_default<'a>(
    cx: &mut JsScope<'a>,
    number: Value<'a>,
    name: Value<'a>,
    lower: Value<'a>,
    upper: Value<'a>,
    fallback: Value<'a>,
) -> bindgen::Result<'a, JSValue> {
    let (number, name, lower, upper, fallback) = (number.to_js(), name.to_js(), lower.to_js(), upper.to_js(), fallback.to_js());
    if number.is_undefined() {
        return Ok(fallback);
    }
    if number.is_number() && lower.is_number() && upper.is_number() {
        let n = number.as_number();
        if n >= lower.as_number() && n <= upper.as_number() && n.is_finite() {
            return Ok(number);
        }
    }
    let heap = &cx.heap;
    let global = heap.global_object();
    if !validate_finite(heap, &number, &name)? {
        return Ok(fallback);
    }
    let n = number.to_number(global)?;
    let (lower, upper) = (lower.to_number(global)?, upper.to_number(global)?);
    if n < lower || n > upper {
        let range = format!(">= {} and <= {}", number_text(global, lower)?, number_text(global, upper)?);
        return Err(out_of_range(heap, &name, &range, &number));
    }
    Ok(number)
}

/// White space and line terminators, as `String.prototype.trim` removes them.
fn is_js_white_space(c: char) -> bool {
    matches!(c, '\u{9}'..='\u{D}' | ' ' | '\u{85}' | '\u{A0}' | '\u{1680}' | '\u{2000}'..='\u{200A}' | '\u{2028}' | '\u{2029}' | '\u{202F}' | '\u{205F}' | '\u{3000}' | '\u{FEFF}')
}

pub(crate) fn validate_port<'a>(cx: &mut JsScope<'a>, port: Value<'a>, name: Value<'a>, allow_zero: Value<'a>) -> bindgen::Result<'a, u16> {
    let (port, name, allow_zero) = (port.to_js(), name.to_js(), allow_zero.to_js());
    let zero_allowed = allow_zero.is_undefined() || allow_zero.to_boolean();
    if port.is_int32() && port.as_int32() <= 0xffff && port.as_int32() >= i32::from(!zero_allowed) {
        return Ok(port.as_int32() as u16);
    }
    let heap = &cx.heap;
    let global = heap.global_object();
    let name = if name.is_undefined() { JSValue::from_utf8(b"Port") } else { name };
    let bad_port = || -> bindgen::Error<'a> {
        let message = (|| -> JsResult<String> {
            Ok(format!(
                "{} should be {} 0 and < 65536. Received {}",
                to_string_safe(global, &name, false)?,
                if zero_allowed { ">=" } else { ">" },
                to_string_safe(global, &port, false)?
            ))
        })();
        match message {
            Ok(message) => heap.error_with_code(ErrorCode::SOCKET_BAD_PORT, format_args!("{message}")),
            Err(error) => error.into(),
        }
    };
    if !port.is_number() && !port.is_string() {
        return Err(bad_port());
    }
    if port.is_string() && port.to_utf8(global)?.trim_matches(is_js_white_space).is_empty() {
        return Err(bad_port());
    }
    let n = port.to_number(global)?;
    if !n.is_finite() || n % 1.0 != 0.0 || !(0.0..=65535.0).contains(&n) || (n == 0.0 && !zero_allowed) {
        return Err(bad_port());
    }
    Ok(n as u16)
}

pub(crate) fn validate_abort_signal<'a>(cx: &mut JsScope<'a>, signal: Value<'a>, name: Value<'a>) -> Unit<'a> {
    let (signal, name) = (signal.to_js(), name.to_js());
    if signal.is_undefined() {
        return Ok(());
    }
    let heap = &cx.heap;
    // An object that is no AbortSignal passes for one if it has a property `aborted`, its own or inherited.
    let Some(object) = signal.get_object() else {
        return Err(invalid_arg_type(heap, &name, "AbortSignal", &signal));
    };
    if !object.has_property(heap.global_object(), "aborted")? {
        return Err(invalid_arg_type(heap, &name, "AbortSignal", &signal));
    }
    Ok(())
}

pub(crate) fn validate_array<'a>(cx: &mut JsScope<'a>, value: Value<'a>, name: Value<'a>, min_length: Value<'a>) -> Unit<'a> {
    let (value, name, min_length) = (value.to_js(), name.to_js(), min_length.to_js());
    if min_length.is_undefined() && value.is_array() {
        return Ok(());
    }
    let heap = &cx.heap;
    let global = heap.global_object();
    if !value.is_array_abstract(global)? {
        return Err(invalid_arg_instance(heap, &name.to_utf8(global)?, "Array", &value));
    }
    let length = value.get(global, "length")?.unwrap_or_default().to_number(global)?;
    let min_length = if min_length.is_undefined() { 0.0 } else { min_length.to_number(global)? };
    if length < min_length {
        let reason = format!("must be longer than {}", number_text(global, min_length)?);
        return Err(invalid_arg_value(heap, &name, &value, &reason));
    }
    Ok(())
}

pub(crate) fn validate_encoding<'a>(cx: &mut JsScope<'a>, data: Value<'a>, encoding: Value<'a>) -> Unit<'a> {
    let (data, encoding) = (data.to_js(), encoding.to_js());
    let heap = &cx.heap;
    let global = heap.global_object();
    // `parseEnumeration<BufferEncodingType>`: only "hex" asks for an even length.
    if !encoding.is_string() || !encoding.to_utf8(global)?.eq_ignore_ascii_case("hex") {
        return Ok(());
    }
    let length = if data.is_string() {
        data.get_length(global)?
    } else if let Some(length) = data.array_buffer_length(global)? {
        length
    } else if data.is_object() {
        data.get_length(global)?
    } else {
        0
    };
    if length % 2 != 0 {
        let reason = format!("is invalid for data of length {length}");
        let message = to_string_safe(global, &encoding, true)?;
        return Err(heap.error_with_code(
            ErrorCode::INVALID_ARG_VALUE,
            format_args!("The argument 'encoding' {reason}. Received {message}"),
        ));
    }
    Ok(())
}

pub(crate) fn validate_buffer<'a>(cx: &mut JsScope<'a>, buffer: Value<'a>, name: Value<'a>) -> Unit<'a> {
    let (buffer, name) = (buffer.to_js(), name.to_js());
    if buffer.is_undefined() || buffer.is_array_buffer_view() {
        return Ok(());
    }
    let heap = &cx.heap;
    let name = if name.is_undefined() { "buffer".to_owned() } else { name.to_utf8(heap.global_object())? };
    Err(invalid_arg_instance(heap, &name, "Buffer, TypedArray, or DataView", &buffer))
}

pub(crate) fn validate_one_of<'a>(cx: &mut JsScope<'a>, value: Value<'a>, name: Value<'a>, one_of: Value<'a>) -> Unit<'a> {
    let (value, name, one_of) = (value.to_js(), name.to_js(), one_of.to_js());
    let heap = &cx.heap;
    let global = heap.global_object();
    if !one_of.is_array() {
        return Err(invalid_arg_type(heap, &JSValue::from_utf8(b"values"), "Array", &one_of));
    }
    let length = one_of.get_length(global)?;
    let mut listed = Vec::with_capacity(length as usize);
    for i in 0..length {
        let element = one_of.get_index(global, i as u32)?;
        if element.is_strict_equal(value.clone(), global)? || element.is_same_value(value.clone(), global)? {
            return Ok(());
        }
        listed.push(element);
    }
    let name = to_string_safe(global, &name, false)?;
    let mut message = format!("The {} '{name}' must be one of: ", if name.contains('.') { "property" } else { "argument" });
    for (i, element) in listed.iter().enumerate() {
        if element.is_string() {
            message.push('\'');
            message.push_str(&element.to_utf8(global)?);
            message.push('\'');
        } else {
            message.push_str(&to_string_safe(global, element, false)?);
        }
        if i + 1 < listed.len() {
            message.push_str(", ");
        }
    }
    message.push_str(&format!(". Received {}", to_string_safe(global, &value, true)?));
    Err(heap.error_with_code(ErrorCode::INVALID_ARG_VALUE, format_args!("{message}")))
}

/// `options`: a sum of kValidateObjectAllowNullable (1), kValidateObjectAllowArray (2) and
/// kValidateObjectAllowFunction (4).
pub(crate) fn validate_object<'a>(cx: &mut JsScope<'a>, value: Value<'a>, name: Value<'a>, options: Value<'a>) -> Unit<'a> {
    const ALLOW_NULLABLE: i32 = 1;
    const ALLOW_ARRAY: i32 = 2;
    const ALLOW_FUNCTION: i32 = 4;
    let (value, name, options) = (value.to_js(), name.to_js(), options.to_js());
    let options = if options.is_int32() { options.as_int32() } else { 0 };
    let heap = &cx.heap;
    if options & ALLOW_ARRAY != 0 && value.is_array() {
        return Ok(());
    }
    if value.is_null() {
        if options & ALLOW_NULLABLE != 0 {
            return Ok(());
        }
        return Err(invalid_arg_type(heap, &name, "object", &value));
    }
    if value.is_callable() {
        if options & ALLOW_FUNCTION != 0 {
            return Ok(());
        }
        return Err(invalid_arg_type(heap, &name, "object", &value));
    }
    if options & ALLOW_ARRAY == 0 && value.is_array_abstract(heap.global_object())? {
        return Err(invalid_arg_type(heap, &name, "object", &value));
    }
    if !value.is_object() {
        return Err(invalid_arg_type(heap, &name, "object", &value));
    }
    Ok(())
}
