//! node:path's native functions on bun_js: what `$bindgen("node_path.bind.ts", …)` answers, called from JavaScript.
//! Each expected value is what JavaScriptCore Bun's `path.posix` and `path.win32` answer for the same expression.

use bun_js::JSGlobalObject;
use bun_sema_standalone as _;

const NAMES: [&str; 11] = [
    "resolve",
    "normalize",
    "isAbsolute",
    "join",
    "relative",
    "toNamespacedPath",
    "dirname",
    "basename",
    "extname",
    "format",
    "parse",
];

/// A realm with `p` and `w`, the functions of path.posix and path.win32 as src/js/node/path.ts puts them together,
/// `cwd`, and `r`, which describes what a function answers or throws.
fn realm(f: impl FnOnce(&JSGlobalObject)) {
    let cwd = std::env::current_dir().expect("a working directory");
    let cwd = cwd.to_str().expect("a working directory in UTF-8");
    // What bun_node_path completes a path that is not absolute with, which the embedder sets.
    bun_paths::fs::FileSystem::init(cwd.as_bytes());
    JSGlobalObject::with_new(|global| {
        let this = global.to_js_value();
        for (object, suffix) in [("p", "Posix"), ("w", "Win32")] {
            let platform = bun_js::JSObject::create(global);
            for name in NAMES {
                let symbol = format!("{name}{suffix}");
                let native = bun_runtime_js::generated_bindgen::native(
                    global,
                    "node_path.bind.ts",
                    &symbol,
                )
                .unwrap_or_else(|| panic!("node_path.bind.ts exports {symbol}"))
                .expect("made");
                platform.put(global, name, native);
            }
            this.put(global, object, platform.to_js());
        }
        this.put(global, "cwd", bun_js::JSValue::from_utf8(cwd.as_bytes()));
        global
            .evaluate_script(
                "globalThis.r = f => { try { const v = f(); return typeof v === 'object' && v !== null ? `${typeof v}:${JSON.stringify(v)}` : `${typeof v}:${String(v)}` } catch (e) { return `throws ${e?.name}: ${e?.message} (${e?.code})` } }",
            )
            .expect("r");
        f(global)
    })
}

/// Each `(expression, what r says of it)`: every one that differs is reported, then the test fails.
fn check(cases: &[(&str, &str)]) {
    realm(|global| {
        let mut failures = Vec::new();
        for &(source, expected) in cases {
            let actual = match global.evaluate_script(&format!("r(() => ({source}))")) {
                Ok(value) => value.to_utf8(global).expect("a string"),
                Err(error) => format!(
                    "uncaught {}",
                    global
                        .take_exception(error)
                        .to_utf8(global)
                        .unwrap_or_default()
                ),
            };
            if actual != expected {
                failures.push(format!(
                    "{source}\n    expected: {expected}\n    actual:   {actual}"
                ));
            }
        }
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
fn natives_by_file_and_symbol() {
    realm(|global| {
        use bun_runtime_js::generated_bindgen::{native, node_path};
        assert!(native(global, "node_path.bind.ts", "basename").is_none());
        assert!(native(global, "node_path", "basenamePosix").is_none());
        assert!(native(global, "fs.bind.ts", "basenamePosix").is_none());
        assert!(node_path::native(global, "parseWin32").is_some());
        // A type is not a value.
        assert!(node_path::native(global, "ParsedPath").is_none());
        // Each call makes a new function, as C++'s js2native does.
        let first = node_path::native(global, "joinPosix").unwrap().unwrap();
        let second = node_path::native(global, "joinPosix").unwrap().unwrap();
        assert!(!first.is_strict_equal(second, global).unwrap());
    });
}

#[test]
fn basename() {
    check(&[
        ("p.basename('/a/b/c.txt')", "string:c.txt"),
        ("p.basename('/a/b/c.txt', '.txt')", "string:c"),
        ("p.basename('/a/b/c.txt', 'c.txt')", "string:c.txt"),
        ("p.basename('/a/b/', 'b')", "string:b"),
        ("p.basename('aaa', 'a')", "string:aa"),
        ("p.basename('/dir/', '')", "string:dir"),
        ("p.basename('/ü/é.txt', '.txt')", "string:é"),
        ("p.basename('é', 'ab')", "string:é"),
        ("p.basename('ab', 'éb')", "string:ab"),
        ("p.basename('\\uD800')", "string:�"),
        ("p.basename(new String('/x/y.js'), new String('.js'))", "string:y"),
        ("p.basename('a\\\\b')", "string:a\\b"),
        ("p.basename(1)", "throws TypeError: The \"path\" property must be of type string, got number (ERR_INVALID_ARG_TYPE)"),
        ("p.basename('a', 1)", "throws TypeError: The \"ext\" property must be of type string, got number (ERR_INVALID_ARG_TYPE)"),
        ("p.basename(1, 2)", "throws TypeError: The \"ext\" property must be of type string, got number (ERR_INVALID_ARG_TYPE)"),
        ("p.basename()", "throws TypeError: The \"path\" property must be of type string, got undefined (ERR_INVALID_ARG_TYPE)"),
        ("p.basename(new String('/x'), null)", "throws TypeError: The \"ext\" property must be of type string, got object (ERR_INVALID_ARG_TYPE)"),
        ("w.basename('C:\\\\a\\\\b.txt')", "string:b.txt"),
        ("w.basename('C:\\\\a\\\\b.txt', '.TXT')", "string:b.txt"),
        ("w.basename('C:\\\\a\\\\b.txt', '.txt')", "string:b"),
        ("w.basename('\\\\\\\\server\\\\share\\\\x')", "string:x"),
        ("w.basename('C:')", "string:"),
        ("w.basename('a/b\\\\c')", "string:c"),
        ("w.basename([])", "throws TypeError: The \"path\" property must be of type string, got array (ERR_INVALID_ARG_TYPE)"),
        ("(() => { const log = []; const s = new String('/a'); s.toString = () => (log.push('path'), '/b'); try { p.basename(s, 1) } catch (e) { log.push(e.code) } return log.join() })()", "string:path,ERR_INVALID_ARG_TYPE"),
        ("(() => { const s = new String('/a/x.js'); s.toString = () => '/b/y.js'; return p.basename(s, '.js') })()", "string:y"),
    ]);
}

#[test]
fn dirname() {
    check(&[
        ("p.dirname('/a/b/c')", "string:/a/b"),
        ("p.dirname('a')", "string:."),
        ("p.dirname('/')", "string:/"),
        ("p.dirname('')", "string:."),
        ("p.dirname('//a')", "string://"),
        ("p.dirname('/ü/é')", "string:/ü"),
        ("w.dirname('C:\\\\a\\\\b')", "string:C:\\a"),
        ("w.dirname('\\\\\\\\s\\\\h\\\\x')", "string:\\\\s\\h\\"),
        ("w.dirname('C:')", "string:C:"),
        ("w.dirname('C:a')", "string:C:"),
        ("p.dirname(null)", "throws TypeError: The \"path\" property must be of type string, got object (ERR_INVALID_ARG_TYPE)"),
        ("p.dirname([])", "throws TypeError: The \"path\" property must be of type string, got array (ERR_INVALID_ARG_TYPE)"),
        ("p.dirname({})", "throws TypeError: The \"path\" property must be of type string, got object (ERR_INVALID_ARG_TYPE)"),
        ("p.dirname(() => 1)", "throws TypeError: The \"path\" property must be of type string, got function (ERR_INVALID_ARG_TYPE)"),
        ("p.dirname(Symbol())", "throws TypeError: The \"path\" property must be of type string, got symbol (ERR_INVALID_ARG_TYPE)"),
        ("p.dirname(1n)", "throws TypeError: The \"path\" property must be of type string, got bigint (ERR_INVALID_ARG_TYPE)"),
        ("w.dirname(true)", "throws TypeError: The \"path\" property must be of type string, got boolean (ERR_INVALID_ARG_TYPE)"),
    ]);
}

#[test]
fn extname() {
    check(&[
        ("p.extname('a.b.c')", "string:.c"),
        ("p.extname('.bashrc')", "string:"),
        ("p.extname('a.')", "string:."),
        ("p.extname('ü.ß')", "string:.ß"),
        ("p.extname('/a.b/c')", "string:"),
        ("w.extname('C:\\\\a.b\\\\c')", "string:"),
        ("w.extname('C:\\\\a\\\\c.d')", "string:.d"),
        ("w.extname(undefined)", "throws TypeError: The \"path\" property must be of type string, got undefined (ERR_INVALID_ARG_TYPE)"),
    ]);
}

#[test]
fn format() {
    check(&[
        ("p.format({ root: '/', dir: '/a', base: 'b.txt' })", "string:/a/b.txt"),
        ("p.format({ name: 'x', ext: 'js' })", "string:x.js"),
        ("p.format({ name: 'x', ext: '.js' })", "string:x.js"),
        ("p.format({ root: '/', name: 'x' })", "string:/x"),
        ("p.format({ root: '/r', dir: '/r' , base: 'b'})", "string:/rb"),
        ("p.format({ dir: 'd', base: '' })", "string:d/"),
        ("p.format({})", "string:"),
        ("p.format({ root: 0, dir: false, base: '', name: 'n', ext: 'e' })", "string:false/n.e"),
        ("p.format({ dir: new String(''), base: 'b' })", "string:b"),
        ("w.format({ dir: 'C:\\\\a', base: 'b' })", "string:C:\\a\\b"),
        ("w.format({ root: 'C:\\\\', base: 'b' })", "string:C:\\b"),
        ("p.format(() => 1)", "string:"),
        ("p.format(null)", "throws TypeError: The \"pathObject\" property must be of type object, got object (ERR_INVALID_ARG_TYPE)"),
        ("p.format([])", "throws TypeError: The \"pathObject\" property must be of type object, got array (ERR_INVALID_ARG_TYPE)"),
        ("p.format('x')", "throws TypeError: The \"pathObject\" property must be of type object, got string (ERR_INVALID_ARG_TYPE)"),
        ("p.format()", "throws TypeError: The \"pathObject\" property must be of type object, got undefined (ERR_INVALID_ARG_TYPE)"),
        ("w.format(1)", "throws TypeError: The \"pathObject\" property must be of type object, got number (ERR_INVALID_ARG_TYPE)"),
        ("p.format({ get root() { throw new RangeError('getter') } })", "throws RangeError: getter (undefined)"),
        ("(() => { const seen = []; p.format(new Proxy({}, { get(_, key) { seen.push(key) } })); return seen.join() })()", "string:root,dir,base,name,ext"),
        ("(() => { const log = []; const s = name => ({ toString() { log.push(name); return name } }); p.format({ root: s('/'), dir: s('d'), base: s('b') }); return log.join() })()", "string:/,d,b"),
        ("(() => { try { p.format({ root: Symbol() }) } catch (e) { return e instanceof TypeError } })()", "boolean:true"),
    ]);
}

#[test]
fn is_absolute() {
    check(&[
        ("p.isAbsolute('/a')", "boolean:true"),
        ("p.isAbsolute('a')", "boolean:false"),
        ("p.isAbsolute('')", "boolean:false"),
        ("p.isAbsolute(new String('/x'))", "boolean:true"),
        ("w.isAbsolute('C:\\\\a')", "boolean:true"),
        ("w.isAbsolute('C:a')", "boolean:false"),
        ("w.isAbsolute('\\\\\\\\s')", "boolean:true"),
        ("w.isAbsolute('/a')", "boolean:true"),
        ("w.isAbsolute('é:\\\\')", "boolean:false"),
        ("p.isAbsolute(1)", "throws TypeError: The \"path\" property must be of type string, got number (ERR_INVALID_ARG_TYPE)"),
        ("w.isAbsolute()", "throws TypeError: The \"path\" property must be of type string, got undefined (ERR_INVALID_ARG_TYPE)"),
    ]);
}

#[test]
fn join() {
    check(&[
        ("p.join('a', 'b', '../c')", "string:a/c"),
        ("p.join()", "string:."),
        ("p.join('')", "string:."),
        ("p.join('', '')", "string:."),
        ("p.join('/a/', '/b/')", "string:/a/b/"),
        ("p.join('a', new String('b'))", "string:a/b"),
        ("p.join('ü', 'é')", "string:ü/é"),
        ("w.join('C:\\\\a', '..', 'b')", "string:C:\\b"),
        ("w.join('\\\\\\\\s', 'h', 'x')", "string:\\\\s\\h\\x"),
        ("w.join('a', '', 'b')", "string:a\\b"),
        ("p.join('a', 1)", "throws TypeError: The \"paths[1]\" property must be of type string, got number (ERR_INVALID_ARG_TYPE)"),
        ("p.join('a', 'b', null)", "throws TypeError: The \"paths[2]\" property must be of type string, got object (ERR_INVALID_ARG_TYPE)"),
        ("p.join({})", "throws TypeError: The \"paths[0]\" property must be of type string, got object (ERR_INVALID_ARG_TYPE)"),
        ("w.join('a', [])", "throws TypeError: The \"paths[1]\" property must be of type string, got array (ERR_INVALID_ARG_TYPE)"),
        ("(() => { const log = []; const s = name => { const o = new String(name); o.toString = () => (log.push(name), name); return o }; try { p.join(s('a'), 1, s('b')) } catch (e) { log.push(e.message) } return log.join() })()", "string:a,b,The \"paths[1]\" property must be of type string, got number"),
    ]);
}

#[test]
fn normalize() {
    check(&[
        ("p.normalize('/a//b/../c/.')", "string:/a/c"),
        ("p.normalize('')", "string:."),
        ("p.normalize('./')", "string:./"),
        ("p.normalize('a/../..')", "string:.."),
        ("p.normalize('é//ü/')", "string:é/ü/"),
        ("w.normalize('C:/a//b\\\\..\\\\c')", "string:C:\\a\\c"),
        ("w.normalize('\\\\\\\\s\\\\h\\\\..')", "string:\\\\s\\h\\"),
        ("w.normalize('')", "string:."),
        ("p.normalize(new String('a//b'))", "string:a/b"),
        ("p.normalize(1)", "throws TypeError: The \"path\" property must be of type string, got number (ERR_INVALID_ARG_TYPE)"),
    ]);
}

#[test]
fn parse() {
    check(&[
        ("p.parse('/home/u/f.txt')", "object:{\"root\":\"/\",\"dir\":\"/home/u\",\"base\":\"f.txt\",\"ext\":\".txt\",\"name\":\"f\"}"),
        ("p.parse('f')", "object:{\"root\":\"\",\"dir\":\"\",\"base\":\"f\",\"ext\":\"\",\"name\":\"f\"}"),
        ("p.parse('')", "object:{\"root\":\"\",\"dir\":\"\",\"base\":\"\",\"ext\":\"\",\"name\":\"\"}"),
        ("p.parse('/')", "object:{\"root\":\"/\",\"dir\":\"/\",\"base\":\"\",\"ext\":\"\",\"name\":\"\"}"),
        ("p.parse('.bashrc')", "object:{\"root\":\"\",\"dir\":\"\",\"base\":\".bashrc\",\"ext\":\"\",\"name\":\".bashrc\"}"),
        ("p.parse('/a/ü.é')", "object:{\"root\":\"/\",\"dir\":\"/a\",\"base\":\"ü.é\",\"ext\":\".é\",\"name\":\"ü\"}"),
        ("w.parse('C:\\\\a\\\\b.c')", "object:{\"root\":\"C:\\\\\",\"dir\":\"C:\\\\a\",\"base\":\"b.c\",\"ext\":\".c\",\"name\":\"b\"}"),
        ("w.parse('\\\\\\\\s\\\\h\\\\f.x')", "object:{\"root\":\"\\\\\\\\s\\\\h\\\\\",\"dir\":\"\\\\\\\\s\\\\h\\\\\",\"base\":\"f.x\",\"ext\":\".x\",\"name\":\"f\"}"),
        ("w.parse('C:')", "object:{\"root\":\"C:\",\"dir\":\"C:\",\"base\":\"\",\"ext\":\"\",\"name\":\"\"}"),
        ("Object.keys(p.parse('a')).join()", "string:root,dir,base,ext,name"),
        ("Object.getPrototypeOf(p.parse('a')) === Object.prototype", "boolean:true"),
        ("p.parse(new String('/x.y')).name", "string:x"),
        ("p.parse(1)", "throws TypeError: The \"path\" property must be of type string, got number (ERR_INVALID_ARG_TYPE)"),
        ("w.parse({})", "throws TypeError: The \"path\" property must be of type string, got object (ERR_INVALID_ARG_TYPE)"),
    ]);
}

#[test]
fn relative() {
    check(&[
        ("p.relative('/a/b/c', '/a/d')", "string:../../d"),
        ("p.relative('/a', '/a')", "string:"),
        ("p.relative('', '')", "string:"),
        ("p.relative('/a/b', '/a/b/c/d')", "string:c/d"),
        ("p.relative('a', 'b')", "string:../b"),
        ("p.relative('/ü', '/é')", "string:../é"),
        ("w.relative('C:\\\\a\\\\b', 'C:\\\\a\\\\c')", "string:..\\c"),
        ("w.relative('C:\\\\a', 'D:\\\\b')", "string:D:\\b"),
        ("w.relative('\\\\\\\\s\\\\h\\\\a', '\\\\\\\\s\\\\h\\\\b')", "string:..\\b"),
        ("p.relative(1, 'a')", "throws TypeError: The \"from\" property must be of type string, got number (ERR_INVALID_ARG_TYPE)"),
        ("p.relative('a', {})", "throws TypeError: The \"to\" property must be of type string, got object (ERR_INVALID_ARG_TYPE)"),
        ("p.relative()", "throws TypeError: The \"from\" property must be of type string, got undefined (ERR_INVALID_ARG_TYPE)"),
        ("w.relative('a', null)", "throws TypeError: The \"to\" property must be of type string, got object (ERR_INVALID_ARG_TYPE)"),
    ]);
}

#[test]
fn resolve() {
    check(&[
        ("p.resolve('/a/b', './c')", "string:/a/b/c"),
        ("p.resolve('/a', '/b', 'c')", "string:/b/c"),
        ("p.resolve(1, '/a')", "string:/a"),
        ("p.resolve('/a', 1)", "throws TypeError: The \"paths[1]\" property must be of type string, got number (ERR_INVALID_ARG_TYPE)"),
        ("p.resolve(new String('/x'), 'y')", "string:/x/y"),
        ("p.resolve('/a', '..', '..', 'é')", "string:/é"),
        ("p.resolve() === cwd", "boolean:true"),
        ("p.resolve('.') === cwd", "boolean:true"),
        ("p.resolve('', '') === cwd", "boolean:true"),
        ("p.resolve('a') === cwd + '/a'", "boolean:true"),
        ("p.resolve('a', '..', 'b') === cwd + '/b'", "boolean:true"),
        ("w.resolve('C:\\\\a', 'b')", "string:C:\\a\\b"),
        ("w.resolve('C:\\\\a', '\\\\\\\\s\\\\h', 'x')", "string:\\\\s\\h\\x"),
        ("w.resolve(1, 'C:\\\\a')", "throws TypeError: The \"paths[0]\" property must be of type string, got number (ERR_INVALID_ARG_TYPE)"),
        ("w.resolve('C:\\\\a', {})", "throws TypeError: The \"paths[1]\" property must be of type string, got object (ERR_INVALID_ARG_TYPE)"),
        ("w.resolve('a') === w.resolve(cwd, 'a')", "boolean:true"),
        ("p.resolve('/a', { toString() { return '/b' } })", "throws TypeError: The \"paths[1]\" property must be of type string, got object (ERR_INVALID_ARG_TYPE)"),
        ("(() => { const log = []; const s = name => { const o = new String(name); o.toString = () => (log.push(name), name); return o }; const resolved = p.resolve(s('x'), 1, s('/y')); return [resolved, ...log].join() })()", "string:/y,x,/y"),
    ]);
}

#[test]
fn to_namespaced_path() {
    check(&[
        ("p.toNamespacedPath(5)", "number:5"),
        ("p.toNamespacedPath('a')", "string:a"),
        ("typeof p.toNamespacedPath(new String('x'))", "string:object"),
        ("p.toNamespacedPath()", "undefined:undefined"),
        ("w.toNamespacedPath('C:\\\\a')", "string:\\\\?\\C:\\a"),
        ("w.toNamespacedPath('C:/a/../b')", "string:\\\\?\\C:\\b"),
        ("w.toNamespacedPath('\\\\\\\\s\\\\h\\\\x')", "string:\\\\?\\UNC\\s\\h\\x"),
        ("w.toNamespacedPath('\\\\\\\\?\\\\C:\\\\a')", "string:\\\\?\\C:\\a"),
        ("w.toNamespacedPath(5)", "number:5"),
        ("w.toNamespacedPath('')", "string:"),
        ("w.toNamespacedPath('a') === w.resolve('a')", "boolean:true"),
        ("w.toNamespacedPath('a') === '\\\\' + cwd.slice(1).replaceAll('/', '\\\\') + '\\\\a'", "boolean:true"),
        ("typeof w.toNamespacedPath(new String(''))", "string:object"),
        ("w.toNamespacedPath(new String('C:\\\\a'))", "string:\\\\?\\C:\\a"),
        ("(() => { const s = new String('ignored'); s.toString = () => 'D:\\\\x'; return w.toNamespacedPath(s) })()", "string:\\\\?\\D:\\x"),
    ]);
}

#[test]
fn names_and_lengths_are_those_of_bound_functions() {
    check(&[
        ("['resolve', 'normalize', 'isAbsolute', 'join', 'relative', 'toNamespacedPath', 'dirname', 'basename', 'extname', 'format', 'parse'].map(name => [p[name].name, p[name].length, String(p[name])].join(':')).join()", "string:bound resolve:1:function resolve() { [native code] },bound normalize:1:function normalize() { [native code] },bound isAbsolute:1:function isAbsolute() { [native code] },bound join:1:function join() { [native code] },bound relative:1:function relative() { [native code] },bound toNamespacedPath:1:function toNamespacedPath() { [native code] },bound dirname:1:function dirname() { [native code] },bound basename:1:function basename() { [native code] },bound extname:1:function extname() { [native code] },bound format:1:function format() { [native code] },bound parse:1:function parse() { [native code] }"),
        ("['resolve', 'normalize', 'isAbsolute', 'join', 'relative', 'toNamespacedPath', 'dirname', 'basename', 'extname', 'format', 'parse'].map(name => [w[name].name, w[name].length, String(w[name])].join(':')).join()", "string:bound resolve:1:function resolve() { [native code] },bound normalize:1:function normalize() { [native code] },bound isAbsolute:1:function isAbsolute() { [native code] },bound join:1:function join() { [native code] },bound relative:1:function relative() { [native code] },bound toNamespacedPath:1:function toNamespacedPath() { [native code] },bound dirname:1:function dirname() { [native code] },bound basename:1:function basename() { [native code] },bound extname:1:function extname() { [native code] },bound format:1:function format() { [native code] },bound parse:1:function parse() { [native code] }"),
        ("Object.getOwnPropertyNames(p.join).join()", "string:length,name"),
        ("(() => { const d = Object.getOwnPropertyDescriptor(p.join, 'name'); return [d.writable, d.enumerable, d.configurable].join() })()", "string:false,false,true"),
        ("p.join.hasOwnProperty('prototype')", "boolean:false"),
    ]);
}
