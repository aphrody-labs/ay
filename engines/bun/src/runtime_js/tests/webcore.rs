//! `Headers`, `Request` and `Response` on bun_js, without the event loop. Each expected value is what JavaScriptCore
//! Bun answers for the same expression (test/js/web/fetch/headers.test.ts, response.test.ts).

use bun_js::JSGlobalObject;
use bun_sema_standalone as _;

fn check(cases: &[(&str, &str)]) {
    JSGlobalObject::with_new(|global| {
        // SAFETY: nothing else reaches the realm while it installs.
        bun_runtime_js::install(unsafe { global.context() }).expect("install");
        for (name, value) in bun_runtime_js::webcore::blob::exports(global).expect("Blob") {
            if name == "Blob" {
                global.to_js_value().put(global, name, value);
            }
        }
        global
            .evaluate_script(
                "globalThis.r = f => { try { const v = f(); return typeof v === 'object' && v !== null ? JSON.stringify(v) : `${typeof v}:${String(v)}` } catch (e) { return `throws ${e?.name}: ${e?.message} (${e?.code})` } }",
            )
            .expect("r");
        let mut failures = Vec::new();
        for &(source, expected) in cases {
            let actual = match global.evaluate_script(&format!("r(() => {{ {source} }})")) {
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

const START: &str =
    "const h = new Headers([['Accept','a'],['Content-Type','c'],['Host','h'],['X-Custom','x'],['X-Other','y']]);";

#[test]
fn iteration_sees_changes() {
    check(&[
        (
            "const h = new Headers([['Set-Cookie','a=1'],['X-Last','1'],['Set-Cookie','b=2'],['Accept','*/*']]); return [...h];",
            r#"[["accept","*/*"],["x-last","1"],["set-cookie","a=1"],["set-cookie","b=2"]]"#,
        ),
        (
            &format!("{START} const i = h.entries(); const s = [i.next().value, i.next().value]; h.set('Aa', 'new'); return [...s, ...i];"),
            r#"[["accept","a"],["content-type","c"],["content-type","c"],["host","h"],["x-custom","x"],["x-other","y"]]"#,
        ),
        (
            &format!("{START} const i = h.keys(); i.next(); h.delete('host'); return [...i];"),
            r#"["content-type","x-custom","x-other"]"#,
        ),
        (
            "const h = new Headers({ a: '1' }); const i = h.entries(); i.next(); i.next(); h.set('b', '2'); return i.next();",
            r#"{"done":true}"#,
        ),
        ("const h = new Headers(); const i = h.keys(); h.set('X-Added', '1'); return [...i];", r#"["x-added"]"#),
        (
            &format!(
                "{START} const s = []; h.forEach((v, k) => {{ s.push(k); if (k === 'accept') {{ h.delete('X-Custom'); h.set('X-Zzz', 'n'); }} }}); return s;"
            ),
            r#"["accept","content-type","host","x-other","x-zzz"]"#,
        ),
        ("new Headers().forEach({})", "throws TypeError: Cannot call callback on a non-function (ERR_INVALID_ARG_TYPE)"),
        (
            "return Object.getPrototypeOf(Object.getPrototypeOf(new Headers().keys())) === Object.getPrototypeOf(Object.getPrototypeOf([].keys()))",
            "boolean:true",
        ),
    ]);
}

#[test]
fn values_and_names() {
    check(&[
        ("new Headers().set('emoji', '😃')", "throws TypeError: Header 'emoji' has invalid value (undefined)"),
        ("new Headers().set('user-agent', '😃')", "throws TypeError: Header 'User-Agent' has invalid value (undefined)"),
        ("return new Headers({ x: 'café' }).get('x')", "string:café"),
        ("new Headers().get('a b')", "throws TypeError: Invalid header name: 'a b' (undefined)"),
        ("const h = new Headers(); h.append('Cookie', 'a=1'); h.append('cookie', 'b=2'); return h.get('cookie')", "string:a=1; b=2"),
        (
            "return new Headers([['user-agent','bun'],['cache-control','public, immutable'],['Cache-Control','no-transform']]).count",
            "number:2",
        ),
        (
            "return Object.keys(new Headers({ 'user-agent': 'bun', 'X-Custom-Header': '1', 'cache-control': 'c' }).toJSON())",
            r#"["user-agent","cache-control","x-custom-header"]"#,
        ),
    ]);
}

#[test]
fn response_redirect() {
    check(&[
        ("return Response.redirect('http://example.com/a b').headers.get('location')", "string:http://example.com/a%20b"),
        ("return Response.redirect('HTTP://U:P@EX.COM:80/p/../q', 307).headers.get('location')", "string:http://U:P@ex.com/q"),
        ("return Response.redirect('http://bücher.example/').headers.get('location')", "string:http://xn--bcher-kva.example/"),
        ("return Response.redirect('/café').headers.get('location')", "string:/café"),
        ("Response.redirect('/€')", "throws TypeError: Header 'Location' has invalid value (undefined)"),
        ("return Response.redirect('url', { status: 308 }).status", "number:308"),
        ("Response.redirect('url', 400)", "throws RangeError: Failed to execute 'redirect' on 'Response': Invalid status code (undefined)"),
        ("return new Response('123', { statusText: 123 }).statusText", "string:123"),
    ]);
}

#[test]
fn blob_bodies() {
    check(&[
        ("return new Response(new Blob(['hi'], { type: 'Text/X' })).headers.get('content-type')", "string:text/x"),
        ("return new Request('http://a/', { method: 'POST', body: new Blob(['abc']) }).headers.get('content-type')", "object:null"),
        ("return new Response(new Blob(['hi'])).bodyUsed", "boolean:false"),
    ]);
}
