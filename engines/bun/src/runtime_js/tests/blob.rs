//! Blob, File, Bun.file and Bun.write on Boa, against what Bun on JavaScriptCore answers.

use bun_js::{JSGlobalObject, JSObject, JSValue};

use bun_sema_standalone as _;

/// A realm with `Blob`, `File` and `Bun.{file,write,stdin,stdout,stderr}`.
fn with_runtime(f: impl FnOnce(&JSGlobalObject)) {
    JSGlobalObject::with_new(|global| {
        let bun = JSObject::create(global);
        for (name, value) in bun_runtime_js::webcore::blob::exports(global).unwrap() {
            match name {
                "Blob" | "File" | "FormData" => global.to_js_value().put(global, name, value),
                _ => bun.put(global, name, value),
            }
        }
        global.to_js_value().put(global, "Bun", bun.to_js());
        // SAFETY: the context is used for this one call, and nothing else holds it.
        boa_wintertc::clone::register(None, unsafe { global.context() }).unwrap();
        f(global)
    });
}

/// What the async function body `source` returns, or `threw <code> <message>`, once its promises settle.
fn run(global: &JSGlobalObject, source: &str) -> String {
    let script = format!(
        "globalThis.out = undefined; (async () => {{ {source} }})().then(v => {{ globalThis.out = String(v) }}, e => {{ globalThis.out = 'threw ' + e.code + ' ' + e.message }})"
    );
    if let Err(error) = global.evaluate_script(&script) {
        panic!("{source}: {:?}", global.take_exception(error).to_utf8(global));
    }
    global.drain_microtasks().unwrap();
    global
        .to_js_value()
        .get(global, "out")
        .unwrap()
        .map(|out| out.to_utf8(global).unwrap())
        .unwrap_or_else(|| panic!("{source}: did not settle"))
}

fn temp_dir(name: &str) -> String {
    let dir = std::env::temp_dir().join(format!("bun-runtime-js-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir.to_str().unwrap().to_owned()
}

#[test]
fn blob_parts_types_and_reads() {
    with_runtime(|global| {
        let cases = [
            (
                "return new Blob(['a', new Uint8Array([98]), new Blob(['c']), new Uint16Array([0x6564]).buffer]).text()",
                "abcde",
            ),
            ("return new Blob(['日本', '語']).size", "9"),
            ("return new Blob([1, 2, 3]).text()", "123"),
            ("return new Blob([['a', 'b'], null, undefined]).text()", "a,b"),
            ("return new Blob().size + ':' + new Blob([]).type", "0:"),
            ("return new Blob(['x'], { type: 'Text/Plain' }).type", "text/plain"),
            ("return new Blob(['x'], { type: 'a\\nb' }).type", ""),
            ("return new Blob(['x'], { type: 'X-Custom/Y' }).type", "x-custom/y"),
            ("return new Blob(['hello world']).slice(6).text()", "world"),
            ("return new Blob(['hello world']).slice(-5, -1).text()", "worl"),
            ("return new Blob(['hello']).slice(1, 3, 'A/B').type", "a/b"),
            ("return new Blob(['hello'], { type: 'text/html' }).slice('X/Y').type", "x/y"),
            ("return new Blob(['hi'], { type: 'text/html' }).slice(0, 1).type", "text/html"),
            ("return new Blob(['abc']).slice(5).size", "0"),
            ("return new Uint8Array(await new Blob(['abc']).arrayBuffer()).join()", "97,98,99"),
            ("const b = await new Blob(['ab']).bytes(); return b.constructor.name + b.length", "Uint8Array2"),
            ("return (await new Blob(['{\"a\":[1]}']).json()).a[0]", "1"),
            ("return new Blob(['\\ufeff{\"b\":2}']).json().then(j => j.b)", "2"),
            ("return new Blob(['']).json()", "threw undefined Unexpected end of JSON input"),
            ("return new Blob([new Uint8Array([0xff, 0xfe, 0x68, 0, 0x69, 0])]).text()", "hi"),
            ("return new Blob([new Uint8Array([0xef, 0xbb, 0xbf, 0x6f, 0x6b])]).text()", "ok"),
            ("return new Blob([new Uint8Array([0x61, 0xff, 0x62])]).text()", "a\u{fffd}b"),
            ("return Object.prototype.toString.call(new Blob())", "[object Blob]"),
            ("return new Blob(['a']).name", "undefined"),
            ("return (await new Blob().exists())", "true"),
            ("const i = Symbol.for('nodejs.util.inspect.custom'); return new Blob(['x'], { type: 'Text/Plain' })[i]()", "Blob (1 bytes) {\n  type: \"text/plain\"\n}"),
            ("const i = Symbol.for('nodejs.util.inspect.custom'); return new Blob()[i]()", "Blob (0 KB)"),
            ("const i = Symbol.for('nodejs.util.inspect.custom'); return new File(['ab'], 'n.txt', { lastModified: 7 })[i]()", "File (2 bytes) {\n  name: \"n.txt\",\n  lastModified: 7\n}"),
            ("const i = Symbol.for('nodejs.util.inspect.custom'); return Bun.file('/a/b.txt')[i]()", "FileRef (\"/a/b.txt\") {\n  type: \"text/plain;charset=utf-8\"\n}"),
            ("const i = Symbol.for('nodejs.util.inspect.custom'); return Blob.prototype[i].name + Object.keys(Blob.prototype).includes(i)", "[nodejs.util.inspect.custom]false"),
        ];
        for (source, expected) in cases {
            assert_eq!(run(global, source), expected, "{source}");
        }
    });
}

#[test]
fn blob_constructor_errors() {
    with_runtime(|global| {
        assert_eq!(
            run(global, "return new Blob('ok')"),
            "threw ERR_INVALID_ARG_TYPE new Blob() expects an Array"
        );
        assert_eq!(run(global, "return new Blob(new Uint8Array([104, 105])).text()"), "hi");
        assert!(run(global, "return Blob()").starts_with("threw"));
    });
}

#[test]
fn file_constructor() {
    with_runtime(|global| {
        let cases = [
            ("const f = new File(['x'], 'a.txt', { lastModified: 5 }); return [f.name, f.lastModified, f.size].join()", "a.txt,5,1"),
            ("const f = new File([], 'a', { type: 'TEXT/CSV' }); return f.type", "text/csv"),
            ("const f = new File([], 'a', { lastModified: null }); return f.lastModified", "0"),
            ("const t = Date.now(); const f = new File([], 'a'); return f.lastModified >= t", "true"),
            ("return new File([], 'a') instanceof File && new File([], 'a') instanceof Blob", "true"),
            ("return new Blob([]) instanceof File", "false"),
            ("return File.prototype === Blob.prototype && File.name + File.length", "File2"),
            ("return new File([], 123).name", "123"),
            ("class F extends File {}; const f = new F(['ab'], 'n'); return (f instanceof F) + ':' + f.size", "true:2"),
            ("const f = new File([], 'a'); f.name = 'b'; f.name = 5; return f.name", "b"),
            ("return new File(['a'])", "threw ERR_INVALID_ARG_TYPE new File(bits, name) expects at least 2 arguments"),
            ("return File([], 'a')", "threw undefined Class constructor File cannot be invoked without 'new'"),
        ];
        for (source, expected) in cases {
            assert_eq!(run(global, source), expected, "{source}");
        }
    });
}

#[test]
fn bun_file_and_write() {
    with_runtime(|global| {
        let dir = temp_dir("file");
        global
            .to_js_value()
            .put(global, "dir", JSValue::from_utf8(dir.as_bytes()));
        let cases = [
            ("return Bun.write(dir + '/a.txt', 'hello')", "5".to_owned()),
            ("return Bun.file(dir + '/a.txt').text()", "hello".to_owned()),
            ("const f = Bun.file(dir + '/a.txt'); return [f.size, f.type, f.name === dir + '/a.txt'].join()", "5,text/plain;charset=utf-8,true".to_owned()),
            ("return Bun.file(dir + '/a.txt').exists()", "true".to_owned()),
            ("return Bun.file(dir).exists()", "false".to_owned()),
            ("return Bun.file(dir + '/a.txt').slice(1, 3).text()", "el".to_owned()),
            ("return Bun.file(dir + '/x.bin').type", "application/octet-stream".to_owned()),
            ("return Bun.file(dir + '/x', { type: 'text/plain' }).type", "text/plain;charset=utf-8".to_owned()),
            ("return Bun.file(dir + '/x', { type: 'Custom/Type' }).type", "custom/type".to_owned()),
            ("return Bun.write(dir + '/deep/er/b.bin', new Uint8Array([1, 2, 3]))", "3".to_owned()),
            ("return (await Bun.file(dir + '/deep/er/b.bin').bytes()).join()", "1,2,3".to_owned()),
            ("return Bun.write(Bun.file(dir + '/copy.txt'), Bun.file(dir + '/a.txt'))", "5".to_owned()),
            ("return Bun.file(dir + '/copy.txt').text()", "hello".to_owned()),
            ("return Bun.write(Bun.file(dir + '/copy.txt').slice(0, 2), Bun.file(dir + '/a.txt'))", "2".to_owned()),
            ("return Bun.write(dir + '/parts.txt', ['a', new Blob(['b'])])", "2".to_owned()),
            ("return Bun.write(dir + '/empty.txt', new Blob([]))", "0".to_owned()),
            ("return Bun.file(dir + '/empty.txt').exists()", "true".to_owned()),
            ("return Bun.file(Uint8Array.from(dir + '/a.txt', c => c.charCodeAt(0))).text()", "hello".to_owned()),
            ("const f = Bun.file(dir + '/w.txt'); await f.write('ww'); return f.text()", "ww".to_owned()),
            ("const s = await Bun.file(dir + '/a.txt').stat(); return [s.size, s.isFile(), s.isDirectory(), s.mtime instanceof Date].join()", "5,true,false,true".to_owned()),
            ("return Bun.file(dir + '/w.txt').delete()", "undefined".to_owned()),
            ("return Bun.file(dir + '/w.txt').exists()", "false".to_owned()),
            (
                "return Bun.file(dir + '/missing.txt').text()",
                format!("threw ENOENT ENOENT: no such file or directory, open '{dir}/missing.txt'"),
            ),
            (
                "return Bun.file(dir + '/missing.txt').delete()",
                format!("threw ENOENT ENOENT: no such file or directory, unlink '{dir}/missing.txt'"),
            ),
            (
                "return Bun.file(dir + '/missing.txt').stat()",
                format!("threw ENOENT ENOENT: no such file or directory, stat '{dir}/missing.txt'"),
            ),
            (
                "return Bun.write(dir + '/no/dir.txt', 'x', { createPath: false })",
                format!("threw ENOENT ENOENT: no such file or directory, open '{dir}/no/dir.txt'"),
            ),
            (
                "return Bun.write(dir + '/out.txt', Bun.file(dir + '/missing.txt'))",
                format!("threw ENOENT ENOENT: no such file or directory, open '{dir}/missing.txt'"),
            ),
            ("return Bun.file(dir).text()", "threw EISDIR Directories cannot be read like files".to_owned()),
            ("return Bun.write(dir + '/m.txt', 'x', { mode: 1000 })", "threw ERR_OUT_OF_RANGE The value of \"mode\" is out of range. It must be >= 0 and <= 511. Received 1000".to_owned()),
            ("return Bun.write(dir + '/m.txt')", "threw ERR_INVALID_ARG_TYPE Bun.write(pathOrFdOrBlob, blob) expects a Blob-y thing to write".to_owned()),
            ("return Bun.write(new Blob(['x']), 'y')", "threw ERR_INVALID_ARG_TYPE Cannot write to a Blob backed by bytes, which are always read-only".to_owned()),
            ("return Bun.write({}, 'y')", "threw ERR_INVALID_ARG_TYPE The \"destination\" argument must be of type path, file descriptor, or Blob. Received an instance of Object".to_owned()),
            ("return Bun.file({})", "threw ERR_INVALID_ARG_TYPE Expected file path string or file descriptor".to_owned()),
            ("return Bun.file(1.5)", "threw ERR_OUT_OF_RANGE The value of \"fd\" is out of range. It must be an integer. Received 1.5".to_owned()),
            ("return Bun.file('a\\0b')", "threw ERR_INVALID_ARG_VALUE The argument 'path' must be a string, Uint8Array, or URL without null bytes. Received \"a\\u0000b\"".to_owned()),
            ("return Bun.write(Bun.file(123), 'x', { createPath: true })", "threw ERR_INVALID_ARG_TYPE Cannot create a directory for a file descriptor".to_owned()),
            ("return Bun.stdout === Bun.stdout && Bun.stdout.type", "application/octet-stream".to_owned()),
        ];
        for (source, expected) in cases {
            assert_eq!(run(global, source), expected, "{source}");
        }
        assert_eq!(std::fs::read_to_string(format!("{dir}/copy.txt")).unwrap(), "he");
        assert_eq!(std::fs::read_to_string(format!("{dir}/parts.txt")).unwrap(), "ab");
        let _ = std::fs::remove_dir_all(&dir);
    });
}

#[test]
fn structured_clone() {
    with_runtime(|global| {
        let cases = [
            (
                "const b = structuredClone(new Blob(['abc'], { type: 'text/plain' })); return [b instanceof Blob, b.type, b.size, await b.text()].join()",
                "true,text/plain,3,abc",
            ),
            (
                "const f = structuredClone(new File(['x'], 'n.txt', { lastModified: 5, type: 'Text/CSV' })); return [f instanceof File, f.name, f.lastModified, f.type].join()",
                "true,n.txt,5,text/csv",
            ),
            (
                "const s = structuredClone(new Blob(['hello']).slice(1, 3)); return [s.size, await s.text()].join()",
                "2,el",
            ),
            (
                "const b = new Blob(['x']); const [c, d] = structuredClone([b, b]); return [c === d, c !== b, structuredClone({ b }).b instanceof Blob].join()",
                "true,true,true",
            ),
            (
                "const f = structuredClone(Bun.file('/nonexistent/x.txt')); return [f instanceof Blob, f.name, f.type].join()",
                "true,/nonexistent/x.txt,text/plain;charset=utf-8",
            ),
            ("return structuredClone(new Blob()).size", "0"),
        ];
        for (source, expected) in cases {
            assert_eq!(run(global, source), expected, "{source}");
        }
    });
}

/// Each expected value is what Bun on JavaScriptCore answers.
#[test]
fn form_data() {
    with_runtime(|global| {
        let multipart = r#"const body = '--x\r\nContent-Disposition: form-data; name="k"\r\n\r\nv\r\n--x\r\nContent-Disposition: form-data; name="f"; filename="a.txt"\r\n\r\nhello\r\n--x--';"#;
        let cases = [
            (
                "const fd = new FormData(); fd.append('a','1'); fd.append('b', new Blob(['xy'], {type:'text/plain'})); fd.append('c', new Blob(['z']), 'c.txt'); const d = new File(['q'], 'orig.bin'); fd.append('d', d); fd.append('a', 2); const b = fd.get('b'); return [typeof b, b instanceof File, b.name, b.type, b.size, b === fd.get('b'), fd.get('c').name, fd.get('d') === d, fd.get('missing'), fd.getAll('a').join('|'), fd.has('a'), fd.has('z')].join()".to_owned(),
                "object,true,,text/plain,2,true,c.txt,true,,1|2,true,false",
            ),
            (
                "const fd = new FormData(); for (const k of ['a','b','a','c','a']) fd.append(k, k); fd.set('a', 'x'); const one = [...fd.keys()].join(''); fd.delete('a'); return one + ':' + [...fd.entries()].map(e => e.join('=')).join('&') + ':' + fd.length".to_owned(),
                "abc:b=b&c=c:2",
            ),
            (
                "const fd = new FormData(); fd.append('k','v'); const seen = []; fd.forEach(function (v, k, p) { seen.push(k + v + (p === fd) + (this == 1)) }, 1); return [...fd].join(';') + ':' + seen.join() + ':' + Object.prototype.toString.call(fd) + ':' + [...fd.values()].join()".to_owned(),
                "k,v:kvtruetrue:[object FormData]:v",
            ),
            (
                "const fd = new FormData(); const e = []; for (const f of [() => fd.append('x'), () => fd.append(), () => fd.append('x','y','z'), () => fd.get()]) { try { f() } catch (x) { e.push(x.name + ' ' + x.message) } } return e.join('|')".to_owned(),
                "TypeError Not enough arguments|TypeError Not enough arguments|TypeError Expected argument to be a Blob.|TypeError Not enough arguments",
            ),
            (
                "const fd = new FormData(); fd.append('a','1'); fd.append('a','2'); fd.append('b','3'); const j = fd.toJSON(); return JSON.stringify(j) + Object.prototype.toString.call(j)".to_owned(),
                r#"{"a":["1","2"],"b":"3"}[object FormData]"#,
            ),
            (
                "const r = await new Blob(['a=1&b=x+y&c=%41&&d'], {type:'application/x-www-form-urlencoded'}).formData(); return [...r].map(e => e.join('=')).join('&')".to_owned(),
                "a=1&b=x y&c=A&d=",
            ),
            (
                format!("{multipart} const r = await new Blob([body], {{type: 'multipart/form-data; boundary=x'}}).formData(); return [r.get('k'), r.get('f') instanceof File, r.get('f').name, r.get('f').type, await r.get('f').text()].join()"),
                "v,true,a.txt,text/plain;charset=utf-8,hello",
            ),
            (
                format!("{multipart} return [[...FormData.from(body, 'x').keys()].join(), [...FormData.from('a=1&b=2')].join(';'), [...FormData.from(Uint8Array.from('q=1', c => c.charCodeAt(0)), '')].join(), [...FormData.from(new Blob(['z=9']))].join()].join(' ')"),
                "k,f a,1;b,2 q,1 z,9",
            ),
            ("return new Blob(['x']).formData()".to_owned(), "threw undefined Invalid encoding"),
            (
                "return new Blob(['x'], {type: 'multipart/form-data; boundary=abc'}).formData()".to_owned(),
                "threw undefined FormData encoding failed: missing final boundary",
            ),
            (
                "return [[...(await new Blob([]).formData())].length, FormData.length, FormData.from.length, typeof FormData.prototype[Symbol.iterator], FormData.prototype[Symbol.iterator] === FormData.prototype.entries].join()".to_owned(),
                "0,0,1,function,true",
            ),
            (
                "return FormData.from(1)".to_owned(),
                "threw ERR_INVALID_ARG_TYPE input must be a string, ArrayBuffer or ArrayBufferView",
            ),
            (
                "const fd = new FormData(); fd.append('f', new File(['x'], 'n.txt')); const c = structuredClone(fd.get('f')); return c.name".to_owned(),
                "n.txt",
            ),
        ];
        for (source, expected) in cases {
            assert_eq!(run(global, &source), expected, "{source}");
        }
    });
}

#[test]
fn object_urls() {
    with_runtime(|global| {
        let cases = [
            (
                "const u = Bun.createObjectURL(new Blob(['hi'])); return /^blob:[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/.test(u)",
                "true",
            ),
            (
                "const u = Bun.createObjectURL(new File(['x'], 'f.txt')); const r = Bun.resolveObjectURL(u); return [r instanceof File, r.name, await r.text(), Bun.resolveObjectURL(u) !== r].join()",
                "true,f.txt,x,true",
            ),
            (
                "const u = Bun.createObjectURL(new Blob([])); Bun.revokeObjectURL(new String(u)); return Bun.resolveObjectURL(u)",
                "undefined",
            ),
            ("return Bun.resolveObjectURL('blob:x') + ':' + Bun.resolveObjectURL(1)", "undefined:undefined"),
            ("return Bun.revokeObjectURL('blob:nope')", "undefined"),
            ("return Bun.createObjectURL({})", "threw ERR_INVALID_ARG_TYPE object must be of type Blob or BuildArtifact"),
            ("return Bun.revokeObjectURL()", "threw ERR_MISSING_ARGS The \"url\" argument must be specified"),
            ("return Bun.revokeObjectURL([])", "threw ERR_INVALID_ARG_TYPE revokeObjectURL expects a string"),
        ];
        for (source, expected) in cases {
            assert_eq!(run(global, source), expected, "{source}");
        }
    });
}

#[test]
fn bun_file_errors_have_node_properties() {
    with_runtime(|global| {
        let dir = temp_dir("errors");
        global
            .to_js_value()
            .put(global, "dir", JSValue::from_utf8(dir.as_bytes()));
        let out = run(
            global,
            "try { await Bun.file(dir + '/nope').text() } catch (e) { return [e.name, e.code, e.errno, e.syscall, e.path === dir + '/nope', e instanceof Error].join() }",
        );
        assert_eq!(out, "Error,ENOENT,-2,open,true,true");
        let _ = std::fs::remove_dir_all(&dir);
    });
}

#[test]
fn last_modified_follows_writes() {
    with_runtime(|global| {
        let dir = temp_dir("mtime");
        global
            .to_js_value()
            .put(global, "dir", JSValue::from_utf8(dir.as_bytes()));
        assert_eq!(
            run(
                global,
                "const f = Bun.file(dir + '/t.txt'); await Bun.write(f, 'a'); const a = f.lastModified; return a > 1e12 && a < 1e14"
            ),
            "true"
        );
        let missing = run(global, "return Bun.file(dir + '/none').lastModified");
        assert_eq!(missing, ((1u64 << 52) - 1).to_string());
        let _ = std::fs::remove_dir_all(&dir);
    });
}

/// `Bun.file(...).writer()`: what JavaScriptCore Bun answers for the same calls.
#[test]
fn file_sink_writes_flushes_and_ends() {
    let dir = temp_dir("sink");
    with_runtime(|global| {
        global.to_js_value().put(global, "dir", JSValue::from_utf8(dir.as_bytes()));
        let cases = [
            (
                "const w = Bun.file(dir + '/a').writer(); return [w.constructor.name, w.write('héllo'), w.write(new Uint8Array([33, 34])), w.write(new Uint8Array([35]).buffer), w.write(''), await Bun.file(dir + '/a').size, w.flush(), await Bun.file(dir + '/a').size, w.flush(), w.end(), w.write('x'), w.end(), await Bun.file(dir + '/a').text()].join()",
                "FileSink,6,2,1,0,0,9,9,0,0,true,0,héllo!\"#",
            ),
            (
                "const w = Bun.file(dir + '/a').writer(); try { w.write(1) } catch (e) { return e.code + ' ' + e.message }",
                "ERR_INVALID_ARG_TYPE write() expects a string, ArrayBufferView, or ArrayBuffer",
            ),
            // No truncation: the writer writes from the start of the file.
            (
                "const w = Bun.file(dir + '/a').writer(); w.write('zz'); w.close(); return Bun.file(dir + '/a').text()",
                "zz\u{FFFD}llo!\"#",
            ),
            // A write of a page or more reaches the file at once.
            (
                "const w = Bun.file(dir + '/big').writer(); const n = w.write('x'.repeat(1 << 20)); return [n, await Bun.file(dir + '/big').size, w.end()].join()",
                "1048576,1048576,0",
            ),
            ("try { Bun.file(dir + '/a').writer(1) } catch (e) { return e.message }", "options must be an object or undefined"),
            (
                "try { new Blob(['x']).writer() } catch (e) { return e.message }",
                "Cannot write to a Blob backed by bytes, which are always read-only",
            ),
            (
                "try { Bun.file(dir).writer() } catch (e) { return e.code + ' ' + e.syscall }",
                "EISDIR open",
            ),
        ];
        let failures: Vec<String> = cases
            .iter()
            .filter_map(|&(source, expected)| {
                let actual = run(global, source);
                (actual != expected).then(|| format!("{source}\n    expected: {expected}\n    actual:   {actual}"))
            })
            .collect();
        assert!(failures.is_empty(), "{}", failures.join("\n"));
    });
    let _ = std::fs::remove_dir_all(&dir);
}

/// What a writer keeps is flushed in the next tick of the event loop.
#[test]
fn file_sink_flushes_in_the_next_tick() {
    let dir = temp_dir("sink-tick");
    let mut context = boa_engine::Context::builder()
        .job_executor(bun_runtime_js::EventLoop::new().expect("an event loop"))
        .build()
        .expect("a realm");
    let global = JSGlobalObject::from_context(&mut context);
    let bun = JSObject::create(global);
    for (name, value) in bun_runtime_js::webcore::blob::exports(global).unwrap() {
        bun.put(global, name, value);
    }
    global.to_js_value().put(global, "Bun", bun.to_js());
    global.to_js_value().put(global, "dir", JSValue::from_utf8(dir.as_bytes()));
    global
        .evaluate_script("globalThis.w = Bun.file(dir + '/t').writer(); w.write('abc');")
        .unwrap();
    // SAFETY: nothing else reaches the realm while its loop runs.
    let context = unsafe { global.context() };
    bun_runtime_js::EventLoop::of(context).run(context).unwrap();
    assert_eq!(std::fs::read_to_string(format!("{dir}/t")).unwrap(), "abc");
    let _ = std::fs::remove_dir_all(&dir);
}
