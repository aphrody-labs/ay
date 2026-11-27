//! `Bun.Glob` on bun_js, called from JavaScript: what `bun_runtime_js::bun_object` puts on `Bun`. Each expected
//! value is what JavaScriptCore Bun answers for the same expression (test/js/bun/glob/proto.test.ts has most).

use bun_js::{JSGlobalObject, JSValue};
use bun_sema_standalone as _;

/// A realm with `Glob`, `dir` (a directory of `a.txt`, `.dot`, `sub/b.txt`), and `r`, which describes what a
/// function answers or throws.
fn realm(f: impl FnOnce(&JSGlobalObject, &str)) {
    // One directory for every test of the process, which only reads it. It is also where a scan without a `cwd`
    // starts, which the embedder sets once.
    static DIR: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    let dir = DIR.get_or_init(|| {
        let dir = std::env::temp_dir().join(format!("bun-runtime-js-glob-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("sub")).expect("a temporary directory");
        for file in ["a.txt", ".dot", "sub/b.txt"] {
            std::fs::write(dir.join(file), "").expect("a file");
        }
        let dir = dir.to_str().expect("a temporary directory in UTF-8").to_owned();
        bun_paths::fs::FileSystem::init(dir.as_bytes());
        dir
    });
    JSGlobalObject::with_new(|global| {
        let this = global.to_js_value();
        for (name, value) in bun_runtime_js::bun_object(global).expect("Bun's properties") {
            this.put(global, name, value);
        }
        this.put(global, "dir", JSValue::from_utf8(dir.as_bytes()));
        global
            .evaluate_script(
                "globalThis.r = f => { try { const v = f(); return typeof v === 'object' && v !== null ? `${typeof v}:${JSON.stringify(v)}` : `${typeof v}:${String(v)}` } catch (e) { return `throws ${e?.name}: ${e?.message} (${e?.code})` } }",
            )
            .expect("r");
        f(global, dir)
    });
}

fn eval(global: &JSGlobalObject, source: &str) -> String {
    match global.evaluate_script(&format!("r(() => ({source}))")) {
        Ok(value) => value.to_utf8(global).expect("a string"),
        Err(error) => format!(
            "uncaught {}",
            global
                .take_exception(error)
                .to_utf8(global)
                .unwrap_or_default()
        ),
    }
}

fn check(cases: &[(&str, &str)]) {
    realm(|global, _| {
        let failures: Vec<String> = cases
            .iter()
            .filter_map(|&(source, expected)| {
                let actual = eval(global, source);
                (actual != expected)
                    .then(|| format!("{source}\n    expected: {expected}\n    actual:   {actual}"))
            })
            .collect();
        assert!(!global.has_exception());
        assert!(
            failures.is_empty(),
            "{} of {} differ:\n{}",
            failures.len(),
            cases.len(),
            failures.join("\n")
        );
    });
}

#[test]
fn class() {
    check(&[
        ("Glob.length", "number:1"),
        ("Glob.name", "string:Glob"),
        ("Object.prototype.toString.call(new Glob('*'))", "string:[object Glob]"),
        ("Object.keys(Glob.prototype).sort()", "object:[\"match\",\"scan\",\"scanSync\"]"),
        ("typeof Glob.prototype.__intrinsic__globScanSync", "string:function"),
        ("Glob.prototype.scan.length", "number:1"),
        ("Glob()", "throws TypeError: Glob constructor cannot be invoked without 'new' (ERR_ILLEGAL_CONSTRUCTOR)"),
    ]);
}

#[test]
fn r#match() {
    check(&[
        ("new Glob('*.js').match('foo.js')", "boolean:true"),
        ("new Glob('*.[tj]s').match('foo.ts')", "boolean:true"),
        ("new Glob('**/*.js').match('src/bar/foo.js')", "boolean:true"),
        ("new Glob('*.js').match('src/foo.js')", "boolean:false"),
        ("new Glob('foo/[bcr]ar/baz').match('foo/bar/baz')", "boolean:true"),
        ("new Glob('*').match('foo/bar')", "boolean:false"),
        ("new Glob(new String('*.md')).match('a.md')", "boolean:true"),
    ]);
}

#[test]
fn arguments_of_the_wrong_type() {
    check(&[
        ("new Glob()", "throws TypeError: pattern must be a string (ERR_INVALID_ARG_TYPE)"),
        ("new Glob(5)", "throws TypeError: pattern must be a string (ERR_INVALID_ARG_TYPE)"),
        ("new Glob('*').match()", "throws TypeError: str must be a string (ERR_INVALID_ARG_TYPE)"),
        ("new Glob('*').match(5)", "throws TypeError: str must be a string (ERR_INVALID_ARG_TYPE)"),
        ("new Glob('*').scanSync(5)", "throws TypeError: GlobScanOptions must be an object (ERR_INVALID_ARG_TYPE)"),
        ("new Glob('*').scan(5)", "throws TypeError: GlobScanOptions must be an object (ERR_INVALID_ARG_TYPE)"),
        ("new Glob('*').scanSync({ cwd: 5 })", "throws TypeError: GlobScanOptions.cwd must be a string (ERR_INVALID_ARG_TYPE)"),
        ("Glob.prototype.match.call({}, 'a')", "throws TypeError: Expected this to be instanceof Glob, but received an instance of Object (ERR_INVALID_THIS)"),
    ]);
}

#[test]
fn scan_sync() {
    check(&[
        ("[...new Glob('*').scanSync(dir)].sort()", "object:[\"a.txt\"]"),
        ("[...new Glob('*').scanSync({ cwd: dir, dot: 1 })].sort()", "object:[\".dot\",\"a.txt\"]"),
        ("[...new Glob('*').scanSync({ cwd: dir, dot: 0 })].sort()", "object:[\"a.txt\"]"),
        ("[...new Glob('*').scanSync({ cwd: dir, onlyFiles: '' })].sort()", "object:[\"a.txt\",\"sub\"]"),
        ("[...new Glob('*').scanSync({ cwd: dir, onlyFiles: null })].sort()", "object:[\"a.txt\"]"),
        ("[...new Glob('**/*.txt').scanSync({ cwd: dir })].sort()", "object:[\"a.txt\",\"sub/b.txt\"]"),
        ("[...new Glob('*').scanSync(new String(dir + '/sub'))]", "object:[\"b.txt\"]"),
        ("[...new Glob('*').scanSync({ cwd: new String(dir + '/sub') })]", "object:[\"b.txt\"]"),
        ("[...new Glob('*').scanSync(Object.assign([], { cwd: dir, dot: true }))].sort()", "object:[\".dot\",\"a.txt\"]"),
        ("[...new Glob('*').scanSync({ cwd: dir, absolute: true })][0] === dir + '/a.txt'", "boolean:true"),
        ("[...new Glob('*.txt').scanSync()]", "object:[\"a.txt\"]"),
        ("Object.prototype.toString.call(new Glob('*').scanSync(dir))", "string:[object Generator]"),
        ("[...new Glob('a\\0').scanSync(dir)]", "throws TypeError: The argument 'pattern' must be a string without null bytes. Received \"a\\u0000\" (ERR_INVALID_ARG_VALUE)"),
        ("[...new Glob('*').scanSync({ cwd: 'a'.repeat(100000) })]", "throws Error: scanSync: invalid `cwd`, longer than 4096 bytes (undefined)"),
        (
            "(() => { try { [...new Glob('*').scanSync(dir + '/missing')] } catch (e) { return [e.message === \"ENOENT: no such file or directory, open '\" + dir + \"/missing'\", e.code, e.syscall, e.errno, e.path === dir + '/missing', e.name, Object.keys(e)] } })()",
            "object:[true,\"ENOENT\",\"open\",-2,true,\"Error\",[\"code\",\"path\",\"syscall\",\"errno\"]]",
        ),
    ]);
}

#[test]
fn options_are_read_in_order_below_object_prototype() {
    check(&[
        (
            "(() => { const read = []; try { new Glob('*').scanSync({ get cwd() { read.push('cwd'); return 'a'.repeat(100000) }, get dot() { read.push('dot'); return false } }) } catch {} return read })()",
            "object:[\"cwd\",\"dot\"]",
        ),
        (
            "(() => { Object.prototype.dot = true; try { return [...new Glob('*').scanSync({ cwd: dir })].sort() } finally { delete Object.prototype.dot } })()",
            "object:[\"a.txt\"]",
        ),
        (
            "[...new Glob('*').scanSync(Object.create({ cwd: dir, dot: true }))].sort()",
            "object:[\".dot\",\"a.txt\"]",
        ),
    ]);
}

#[test]
fn scan_settles_its_promise() {
    realm(|global, _| {
        global
            .evaluate_script(
                "globalThis.found = []; globalThis.failed = undefined;
                 (async () => { for await (const path of new Glob('**/*.txt').scan(dir)) found.push(path) })();
                 (async () => { for await (const path of new Glob('*').scan(dir + '/missing')) found.push(path) })().catch(e => failed = e);",
            )
            .expect("scan");
        global.drain_microtasks().expect("the jobs");
        assert_eq!(
            eval(global, "found.sort()"),
            "object:[\"a.txt\",\"sub/b.txt\"]"
        );
        // A walk that fails rejects, as JavaScriptCore's does.
        assert_eq!(
            eval(global, "failed && [failed.code, failed.syscall, failed.errno]"),
            "object:[\"ENOENT\",\"open\",-2]"
        );
    });
}
