//! src/js/internal/validators.ts's natives on bun_js: what `$bindgen("node_validators.bind.ts", …)` answers. Each
//! expected error is what NodeValidatorBodies.h and ErrorCode.cpp throw for the same call.

use bun_js::JSGlobalObject;
use bun_sema_standalone as _;

const NAMES: [&str; 13] = [
    "validateInteger",
    "validateNumber",
    "validateInt32",
    "validateUint32",
    "validateFiniteNumber",
    "checkRangesOrGetDefault",
    "validatePort",
    "validateObject",
    "validateBuffer",
    "validateAbortSignal",
    "validateArray",
    "validateOneOf",
    "validateEncoding",
];

/// A realm with each validator as a global, and `r`, which describes what a call answers or throws.
fn check(cases: &[(&str, &str)]) {
    JSGlobalObject::with_new(|global| {
        let this = global.to_js_value();
        for name in NAMES {
            let native = bun_runtime_js::generated_bindgen::native(global, "node_validators.bind.ts", name)
                .unwrap_or_else(|| panic!("node_validators.bind.ts exports {name}"))
                .expect("made");
            this.put(global, name, native);
        }
        global
            .evaluate_script(
                "globalThis.r = f => { try { return `${typeof f()}:${String(f())}` } catch (e) { return `throws ${e?.name}: ${e?.message} (${e?.code})` } }",
            )
            .expect("r");
        let mut failures = Vec::new();
        for &(source, expected) in cases {
            let actual = match global.evaluate_script(&format!("r(() => ({source}))")) {
                Ok(value) => value.to_utf8(global).expect("a string"),
                Err(error) => format!("uncaught {}", global.take_exception(error).to_utf8(global).unwrap_or_default()),
            };
            if actual != expected {
                failures.push(format!("{source}\n    expected: {expected}\n    actual:   {actual}"));
            }
        }
        assert!(failures.is_empty(), "{} of {} differ:\n{}", failures.len(), cases.len(), failures.join("\n"));
    });
}

#[test]
fn integers_and_numbers() {
    check(&[
        ("validateInteger(3, 'n')", "undefined:undefined"),
        ("validateInteger(1.5, 'n')", "throws RangeError: The value of \"n\" is out of range. It must be an integer. Received 1.5 (ERR_OUT_OF_RANGE)"),
        ("validateInteger('x', 'n')", "throws TypeError: The \"n\" argument must be of type number. Received type string ('x') (ERR_INVALID_ARG_TYPE)"),
        ("validateInteger(5, 'n', 0, 3)", "throws RangeError: The value of \"n\" is out of range. It must be >= 0 && <= 3. Received 5 (ERR_OUT_OF_RANGE)"),
        ("validateInteger(2 ** 60, 'n')", "throws RangeError: The value of \"n\" is out of range. It must be >= -9007199254740991 && <= 9007199254740991. Received 1_152_921_504_606_847_000 (ERR_OUT_OF_RANGE)"),
        ("validateNumber(NaN, 'n')", "undefined:undefined"),
        ("validateNumber(NaN, 'n', 0)", "throws RangeError: The value of \"n\" is out of range. It must be >= 0. Received NaN (ERR_OUT_OF_RANGE)"),
        ("validateNumber(1n, 'opts.n')", "throws TypeError: The \"opts.n\" property must be of type number. Received type bigint (1n) (ERR_INVALID_ARG_TYPE)"),
        ("validateInt32(2 ** 31, 'i')", "throws RangeError: The value of \"i\" is out of range. It must be >= -2147483648 && <= 2147483647. Received 2147483648 (ERR_OUT_OF_RANGE)"),
        ("validateUint32(0, 'u', true)", "throws RangeError: The value of \"u\" is out of range. It must be >= 1 && <= 4294967295. Received 0 (ERR_OUT_OF_RANGE)"),
        ("validateUint32(-0, 'u')", "undefined:undefined"),
        ("validateFiniteNumber(NaN, 'x')", "boolean:false"),
        ("validateFiniteNumber(undefined, 'x')", "boolean:false"),
        ("validateFiniteNumber(1, 'x')", "boolean:true"),
        ("validateFiniteNumber(-Infinity, 'x')", "throws RangeError: The value of \"x\" is out of range. It must be a finite number. Received -Infinity (ERR_OUT_OF_RANGE)"),
        ("checkRangesOrGetDefault(undefined, 'x', 0, 10, 5)", "number:5"),
        ("checkRangesOrGetDefault(NaN, 'x', 0, 10, 5)", "number:5"),
        ("checkRangesOrGetDefault(7, 'x', 0, 10, 5)", "number:7"),
        ("checkRangesOrGetDefault(20, 'x', 0, 10)", "throws RangeError: The value of \"x\" is out of range. It must be >= 0 and <= 10. Received 20 (ERR_OUT_OF_RANGE)"),
    ]);
}

#[test]
fn ports_objects_and_lists() {
    check(&[
        ("validatePort(8080)", "number:8080"),
        ("validatePort(' 80 ')", "number:80"),
        ("validatePort(70000)", "throws RangeError: Port should be >= 0 and < 65536. Received 70000 (ERR_SOCKET_BAD_PORT)"),
        ("validatePort(0, 'port', false)", "throws RangeError: port should be > 0 and < 65536. Received 0 (ERR_SOCKET_BAD_PORT)"),
        ("validatePort('  ')", "throws RangeError: Port should be >= 0 and < 65536. Received    (ERR_SOCKET_BAD_PORT)"),
        ("validateObject({}, 'o')", "undefined:undefined"),
        ("validateObject([], 'o')", "throws TypeError: The \"o\" argument must be of type object. Received an instance of Array (ERR_INVALID_ARG_TYPE)"),
        ("validateObject([], 'o', 2)", "undefined:undefined"),
        ("validateObject(null, 'o', 1)", "undefined:undefined"),
        ("validateObject(() => {}, 'o')", "throws TypeError: The \"o\" argument must be of type object. Received function  (ERR_INVALID_ARG_TYPE)"),
        ("validateObject(1, 'o')", "throws TypeError: The \"o\" argument must be of type object. Received type number (1) (ERR_INVALID_ARG_TYPE)"),
        ("validateBuffer(new Uint8Array(1), 'b')", "undefined:undefined"),
        ("validateBuffer(1, 'b')", "throws TypeError: The \"b\" argument must be an instance of Buffer, TypedArray, or DataView. Received type number (1) (ERR_INVALID_ARG_TYPE)"),
        ("validateAbortSignal({ aborted: false }, 's')", "undefined:undefined"),
        ("validateAbortSignal(1, 's')", "throws TypeError: The \"s\" argument must be of type AbortSignal. Received type number (1) (ERR_INVALID_ARG_TYPE)"),
        ("validateArray([1], 'a', 1)", "undefined:undefined"),
        ("validateArray(new Proxy([], {}), 'a')", "undefined:undefined"),
        ("validateArray({}, 'a')", "throws TypeError: The \"a\" argument must be an instance of Array. Received an instance of Object (ERR_INVALID_ARG_TYPE)"),
        ("validateOneOf('a', 'mode', ['a', 'b'])", "undefined:undefined"),
        ("validateOneOf(NaN, 'mode', [NaN])", "undefined:undefined"),
        ("validateOneOf('c', 'mode', ['a', 'b', 3])", "throws TypeError: The argument 'mode' must be one of: 'a', 'b', 3. Received 'c' (ERR_INVALID_ARG_VALUE)"),
        ("validateOneOf('c', 'options.mode', 'ab')", "throws TypeError: The \"values\" argument must be of type Array. Received type string ('ab') (ERR_INVALID_ARG_TYPE)"),
        ("validateEncoding('abc', 'hex')", "throws TypeError: The argument 'encoding' is invalid for data of length 3. Received 'hex' (ERR_INVALID_ARG_VALUE)"),
        ("validateEncoding('abcd', 'HEX')", "undefined:undefined"),
        ("validateEncoding('abc', 'utf8')", "undefined:undefined"),
    ]);
}
