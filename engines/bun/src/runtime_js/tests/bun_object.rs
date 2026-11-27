//! The functions `bun_runtime_js::bun_object` puts on `Bun`, called from JavaScript. Each expected value is what
//! JavaScriptCore Bun answers for the same expression.

use bun_js::JSGlobalObject;
use bun_sema_standalone as _;

/// Evaluates each source with `Bun` in scope: a string of the value, or `throws <name>: <message> (<code>)`.
fn check(cases: &[(&str, &str)]) {
    JSGlobalObject::with_new(|global| {
        let bun = bun_js::JSObject::create(global);
        for (name, value) in bun_runtime_js::bun_object(global).expect("Bun's properties") {
            bun.put(global, name, value);
        }
        global.to_js_value().put(global, "Bun", bun.to_js());
        let failures: Vec<String> = cases
            .iter()
            .filter_map(|&(source, expected)| {
                let wrapped = format!(
                    "(() => {{ try {{ const v = {source}; return v === null ? 'null' : String(v) }} catch (e) {{ return `throws ${{e.name}}: ${{e.message}} (${{e.code}})` }} }})()"
                );
                let actual = global
                    .evaluate_script(&wrapped)
                    .expect("caught")
                    .to_utf8(global)
                    .expect("a string");
                (actual != expected).then(|| format!("{source}: {actual}, not {expected}"))
            })
            .collect();
        assert!(failures.is_empty(), "{}", failures.join("\n"));
    });
}

#[test]
fn deep_equals() {
    check(&[
        ("Bun.deepEquals.length", "2"),
        ("Bun.deepEquals.name", "deepEquals"),
        ("Bun.deepEquals(1)", "throws TypeError: Expected 2 values to compare (undefined)"),
        ("Bun.deepEquals({ a: 1 }, { a: 1 })", "true"),
        ("Bun.deepEquals({ a: 1, b: undefined }, { a: 1 }, true)", "false"),
        (
            "(() => { class A { constructor() { this.x = 1 } } return Bun.deepEquals(new A, { x: 1 }, true, true) })()",
            "true",
        ),
        // Only `true` is strict.
        (
            "(() => { class A { constructor() { this.x = 1 } } return Bun.deepEquals(new A, { x: 1 }, 1) })()",
            "true",
        ),
    ]);
}

#[test]
fn sleep_sync() {
    check(&[
        ("Bun.sleepSync.length", "1"),
        ("Bun.sleepSync()", "throws TypeError: milliseconds must be a number (ERR_INVALID_ARG_TYPE)"),
        ("Bun.sleepSync('1')", "throws TypeError: milliseconds must be a number (ERR_INVALID_ARG_TYPE)"),
        (
            "Bun.sleepSync(-1)",
            "throws TypeError: argument to sleepSync must not be negative, got -1 (ERR_INVALID_ARG_TYPE)",
        ),
        ("Bun.sleepSync(0)", "undefined"),
        ("Bun.sleepSync(NaN)", "undefined"),
        ("(() => { const start = Date.now(); Bun.sleepSync(20); return Date.now() - start >= 19 })()", "true"),
    ]);
}

#[test]
fn which() {
    check(&[
        ("Bun.which.length", "1"),
        ("Bun.which('sh', { PATH: '/bin' })", "/bin/sh"),
        ("Bun.which('definitely-not-a-command-xyz')", "null"),
        ("Bun.which(null)", "null"),
        ("Bun.which('sh', { PATH: '/nonexistent' })", "null"),
        ("Bun.which('sh', { PATH: '/nonexistent:/bin' })", "/bin/sh"),
        ("Bun.which('sh', { PATH: '/bin', cwd: '/' })", "/bin/sh"),
    ]);
}

#[test]
fn unsafe_object() {
    check(&[
        ("Bun.unsafe.gcAggressionLevel()", "0"),
        ("Bun.unsafe.gcAggressionLevel(1)", "0"),
        ("Bun.unsafe.gcAggressionLevel(7)", "1"),
        ("Bun.unsafe.gcAggressionLevel(0)", "1"),
        ("Bun.unsafe.arrayBufferToString(new Uint8Array([104, 105, 233]))", "hié"),
        ("Bun.unsafe.arrayBufferToString(new Uint16Array([104, 0x263a]))", "h☺"),
        ("Bun.unsafe.arrayBufferToString(new Uint8Array([104, 105]).buffer)", "hi"),
        ("Bun.unsafe.arrayBufferToString(1)", "throws TypeError: Expected an ArrayBuffer (ERR_INVALID_ARG_TYPE)"),
        (
            "Bun.unsafe.setJITPolicy(0)",
            "throws RangeError: The value of \"scale\" is out of range. It must be a finite number >= 1. Received 0 (ERR_OUT_OF_RANGE)",
        ),
        ("Bun.unsafe.setJITPolicy(2)", "undefined"),
        ("Bun.unsafe.setJITPolicy()", "throws TypeError: scale must be a number (ERR_INVALID_ARG_TYPE)"),
        ("typeof Bun.unsafe.memoryFootprint()", "number"),
        ("Bun.unsafe.mimallocDump.length", "0"),
    ]);
}
