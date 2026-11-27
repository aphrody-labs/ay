//! `DOMException` and the statics of `AbortSignal` (web/abort.js) after `bun_runtime_js::install`. Each expected value
//! is what JavaScriptCore Bun answers for the same expression.

use bun_js::JSGlobalObject;
use bun_runtime_js::EventLoop;
use bun_sema_standalone as _;

/// Evaluates each source in a realm on the event loop, runs the loop, then reads `String(out)`.
fn check(cases: &[(&str, &str)]) {
    bun_core::output::stdio::init();
    let mut failures = Vec::new();
    for &(source, expected) in cases {
        let mut context = boa_engine::Context::builder()
            .job_executor(EventLoop::new().expect("an event loop"))
            .build()
            .expect("a realm");
        boa_runtime::abort::register(None, &mut context).expect("AbortController");
        bun_runtime_js::install(&mut context).expect("install");
        let global = JSGlobalObject::from_context(&mut context);
        let wrapped = format!(
            "var out; try {{ const v = {source}; out = v === null ? 'null' : String(v) }} catch (e) {{ out = `throws ${{e.name}}: ${{e.message}} (${{e.code}})` }}"
        );
        global.evaluate_script(&wrapped).expect("caught");
        // SAFETY: nothing else reaches the realm while its loop runs, but what the loop calls.
        let context = unsafe { global.context() };
        EventLoop::of(context).run(context).expect("the loop");
        let actual = global
            .evaluate_script("String(out)")
            .expect("out")
            .to_utf8(global)
            .expect("a string");
        if actual != expected {
            failures.push(format!("{source}: {actual}, not {expected}"));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn dom_exception() {
    check(&[
        ("typeof DOMException", "function"),
        ("DOMException.length", "0"),
        ("DOMException.name", "DOMException"),
        ("new DOMException('m', 'AbortError').code", "20"),
        ("new DOMException('m').name", "Error"),
        ("new DOMException().message", ""),
        ("new DOMException('m', 'NotFoundError') instanceof Error", "true"),
        ("Object.prototype.toString.call(new DOMException())", "[object DOMException]"),
        ("DOMException.ABORT_ERR", "20"),
        ("DOMException.prototype.TIMEOUT_ERR", "23"),
        ("Object.keys(DOMException).length", "25"),
        ("new DOMException('m', { name: 'DataCloneError', cause: 1 }).cause", "1"),
        ("new DOMException('m', { name: 'DataCloneError' }).code", "25"),
        ("String(new DOMException('x', 'SyntaxError'))", "SyntaxError: x"),
        (
            "DOMException()",
            "throws TypeError: Use `new DOMException(...)` instead of `DOMException(...)` (ERR_ILLEGAL_CONSTRUCTOR)",
        ),
        ("Object.getOwnPropertyNames(new DOMException('m','n')).join()", ""),
    ]);
}

#[test]
fn abort_signal() {
    check(&[
        ("AbortSignal.abort().aborted", "true"),
        ("AbortSignal.abort().reason.name", "AbortError"),
        ("AbortSignal.abort().reason.message", "The operation was aborted."),
        ("AbortSignal.abort().reason.code", "20"),
        ("AbortSignal.abort(1).reason", "1"),
        (
            "(() => { const c = new AbortController(); c.abort(); return c.signal.reason instanceof DOMException })()",
            "true",
        ),
        ("AbortSignal.timeout(10).aborted", "false"),
        ("AbortSignal.any([]).aborted", "false"),
        ("AbortSignal.any([AbortSignal.abort(3)]).reason", "3"),
        (
            "(() => { const c = new AbortController(); const s = AbortSignal.any([c.signal]); c.abort(5); return s.aborted + ':' + s.reason })()",
            "true:5",
        ),
        (
            "(() => { const c = new AbortController(); const s = AbortSignal.any([AbortSignal.any([c.signal])]); c.abort(6); return s.aborted + ':' + s.reason })()",
            "true:6",
        ),
        (
            "AbortSignal.timeout(-1)",
            "throws TypeError: Value -1 is outside the range [0, 9007199254740991] (undefined)",
        ),
        (
            "AbortSignal.timeout('a')",
            "throws TypeError: Value NaN is outside the range [0, 9007199254740991] (undefined)",
        ),
        ("AbortSignal.any(1)", "throws TypeError: Type error (undefined)"),
        (
            "AbortSignal.any([1])",
            "throws TypeError: The \"signals[0]\" argument must be an instance of AbortSignal. Received type number (1) (ERR_INVALID_ARG_TYPE)",
        ),
        // The timer does not keep the loop alive: a second timer does, and `out` is set again when the first fires.
        (
            "(() => { const s = AbortSignal.timeout(5); s.addEventListener('abort', () => { out = [s.reason.name, s.reason.message, s.reason.code].join() }); setTimeout(() => {}, 100); return 'pending' })()",
            "TimeoutError,The operation timed out.,23",
        ),
        ("(() => { AbortSignal.timeout(60000); return 'the loop ends' })()", "the loop ends"),
    ]);
}
